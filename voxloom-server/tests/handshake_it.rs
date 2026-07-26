//! End-to-end integration tests for the Phase 3 server: a real TLS client drives
//! the handshake, then exercises the UDP crypto association + loopback and the
//! TCP tunnel fallback. This is the async proof for the server crate; the strict
//! §20 conformance judge (`SimulatedMumbleClient`) is the verifier deliverable in
//! `voxloom-testkit` (separate commit, R2).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{TcpStream, UdpSocket};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use voxloom_crypto::CryptState;
use voxloom_protocol::messages::{tcp, udp};
use voxloom_protocol::{
    ControlMessage, UdpMessage, decode_frame, decode_udp, encode_frame, encode_udp, parse_frame,
};
use voxloom_server::config::ServerConfig;
use voxloom_server::server::{Server, ServerHandle};
use voxloom_server::tls::{self, Identity};

const LOOPBACK_TARGET: u32 = 31;

/// Start a server on ephemeral 127.0.0.1 TCP/UDP ports.
async fn start_server() -> ServerHandle {
    tls::install_crypto_provider();
    let identity = Identity::self_signed(vec!["localhost".to_string()]).expect("identity");
    let addr = "127.0.0.1:0".parse().expect("addr");
    let server = Server::bind(ServerConfig::default(), identity, addr, addr)
        .await
        .expect("bind server");
    server.spawn().expect("spawn server")
}

/// A rustls client config that trusts any server certificate (local test only).
fn client_config() -> Arc<ClientConfig> {
    let algorithms = rustls::crypto::ring::default_provider().signature_verification_algorithms;
    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(TrustAnyServer { algorithms }))
        .with_no_client_auth();
    Arc::new(config)
}

/// TLS-connect to the server and split the stream into framed read/write halves.
async fn connect(handle: &ServerHandle) -> (FramedReader, WriteHalf<TlsStream<TcpStream>>) {
    let tcp = TcpStream::connect(handle.tcp_addr)
        .await
        .expect("tcp connect");
    tcp.set_nodelay(true).ok();
    let connector = TlsConnector::from(client_config());
    let name = ServerName::try_from("localhost").expect("server name");
    let tls = connector.connect(name, tcp).await.expect("tls connect");
    let (read, write) = tokio::io::split(tls);
    (FramedReader::new(read), write)
}

async fn send(writer: &mut WriteHalf<TlsStream<TcpStream>>, message: &ControlMessage) {
    let mut framed = Vec::new();
    encode_frame(message, &mut framed).expect("encode frame");
    writer.write_all(&framed).await.expect("write");
    writer.flush().await.expect("flush");
}

/// Drive Version + Authenticate and collect the handshake up to and including
/// ServerConfig.
async fn do_handshake(
    reader: &mut FramedReader,
    writer: &mut WriteHalf<TlsStream<TcpStream>>,
    username: &str,
) -> Vec<ControlMessage> {
    send(
        writer,
        &ControlMessage::Version(tcp::Version {
            release: Some("test-client".to_string()),
            version_v2: Some((1u64 << 48) | (5u64 << 32)),
            ..Default::default()
        }),
    )
    .await;
    send(
        writer,
        &ControlMessage::Authenticate(tcp::Authenticate {
            username: Some(username.to_string()),
            opus: Some(true),
            ..Default::default()
        }),
    )
    .await;

    let mut collected = Vec::new();
    while let Ok(Some(message)) = tokio::time::timeout(Duration::from_secs(5), reader.next()).await
    {
        let is_config = matches!(message, ControlMessage::ServerConfig(_));
        collected.push(message);
        if is_config {
            break;
        }
    }
    collected
}

/// Index of the first message matching a variant predicate.
fn index_of(messages: &[ControlMessage], pred: impl Fn(&ControlMessage) -> bool) -> usize {
    messages
        .iter()
        .position(pred)
        .expect("expected message kind present in handshake")
}

#[tokio::test]
async fn full_handshake_ordering_over_tls() {
    let handle = start_server().await;
    let (mut reader, mut writer) = connect(&handle).await;
    let messages = do_handshake(&mut reader, &mut writer, "alice").await;

    // The server's own Version arrives first (sent on TLS-encrypted).
    assert!(
        matches!(messages.first(), Some(ControlMessage::Version(_))),
        "first server message must be Version, got {:?}",
        messages.first()
    );

    let crypt = index_of(&messages, |m| matches!(m, ControlMessage::CryptSetup(_)));
    let codec = index_of(&messages, |m| matches!(m, ControlMessage::CodecVersion(_)));
    let channel = index_of(&messages, |m| matches!(m, ControlMessage::ChannelState(_)));
    let user = index_of(&messages, |m| matches!(m, ControlMessage::UserState(_)));
    let sync = index_of(&messages, |m| matches!(m, ControlMessage::ServerSync(_)));
    let config = index_of(&messages, |m| matches!(m, ControlMessage::ServerConfig(_)));

    assert!(crypt < codec, "CryptSetup before CodecVersion");
    assert!(codec < channel, "CodecVersion before ChannelState");
    assert!(channel < user, "ChannelState before self UserState");
    assert!(user < sync, "self UserState before ServerSync (§20 inv 6)");
    assert!(sync < config, "ServerSync before ServerConfig");

    // ServerSync must carry a session id (the client learns its own session).
    match &messages[sync] {
        ControlMessage::ServerSync(s) => assert!(s.session.is_some(), "ServerSync.session set"),
        _ => unreachable!(),
    }
    // The self UserState names the authenticated user in the root channel (0).
    match &messages[user] {
        ControlMessage::UserState(u) => {
            assert_eq!(u.name.as_deref(), Some("alice"));
            assert_eq!(u.channel_id, Some(0));
        }
        _ => unreachable!(),
    }

    handle.shutdown();
}

/// The client's OCB2 state derived from the server's CryptSetup.
fn client_crypt(setup: &tcp::CryptSetup) -> (CryptState, u32) {
    let key: [u8; 16] = setup.key.clone().expect("key").try_into().expect("key len");
    let server_nonce: [u8; 16] = setup
        .server_nonce
        .clone()
        .expect("server nonce")
        .try_into()
        .expect("nonce len");
    let client_nonce: [u8; 16] = setup
        .client_nonce
        .clone()
        .expect("client nonce")
        .try_into()
        .expect("nonce len");
    // Client encrypts C2S with client_nonce, decrypts S2C with server_nonce.
    (CryptState::new(&key, &client_nonce, &server_nonce), 0)
}

fn find_crypt_setup(messages: &[ControlMessage]) -> tcp::CryptSetup {
    for message in messages {
        if let ControlMessage::CryptSetup(setup) = message {
            return setup.clone();
        }
    }
    panic!("CryptSetup missing from handshake");
}

fn session_of(messages: &[ControlMessage]) -> u32 {
    for message in messages {
        if let ControlMessage::ServerSync(sync) = message {
            return sync.session.expect("session");
        }
    }
    panic!("ServerSync missing");
}

#[tokio::test]
async fn udp_association_and_loopback() {
    let handle = start_server().await;
    let udp_addr = handle.udp_addr;
    let (mut reader, mut writer) = connect(&handle).await;
    let messages = do_handshake(&mut reader, &mut writer, "bob").await;

    let (mut crypt, _) = client_crypt(&find_crypt_setup(&messages));
    let my_session = session_of(&messages);

    // Send an encrypted loopback audio packet (target 31) over UDP.
    let opus = vec![0xAAu8, 0xBB, 0xCC, 0xDD];
    let audio = udp::Audio {
        header: Some(udp::audio::Header::Target(LOOPBACK_TARGET)),
        sender_session: 0,
        frame_number: 42,
        opus_data: opus.clone(),
        ..Default::default()
    };
    let plaintext = encode_udp(&UdpMessage::Audio(audio));
    let sealed = crypt.encrypt(&plaintext).expect("encrypt");

    let sock = UdpSocket::bind("127.0.0.1:0").await.expect("bind udp");
    sock.send_to(&sealed, udp_addr).await.expect("send udp");

    // The server should reflect it back, encrypted, to our address.
    let mut buf = vec![0u8; 2048];
    let (len, _from) = tokio::time::timeout(Duration::from_secs(5), sock.recv_from(&mut buf))
        .await
        .expect("no UDP reply before timeout")
        .expect("recv");
    let reply_plain = crypt.decrypt(&buf[..len]).expect("decrypt reply");
    match decode_udp(&reply_plain).expect("decode reply") {
        UdpMessage::Audio(reflected) => {
            assert_eq!(
                reflected.sender_session, my_session,
                "stamped with our session"
            );
            assert_eq!(reflected.opus_data, opus, "opus payload preserved");
            assert!(
                matches!(reflected.header, Some(udp::audio::Header::Context(0))),
                "server->client audio uses context, not target"
            );
        }
        other => panic!("expected reflected Audio, got {other:?}"),
    }

    handle.shutdown();
}

/// A real client reads `Ping.good` as `uiRemoteGood` and drops to TCP mode when
/// it is still zero after 20 seconds, so the reply must count the datagrams we
/// actually decrypted.
/// REF: references/mumble/src/mumble/ServerHandler.cpp : `ServerHandler::message`,
///   `TCPMessageType::Ping`.
#[tokio::test]
async fn tcp_ping_reply_reports_udp_crypt_counters() {
    let handle = start_server().await;
    let udp_addr = handle.udp_addr;
    let (mut reader, mut writer) = connect(&handle).await;
    let messages = do_handshake(&mut reader, &mut writer, "dave").await;
    let (mut crypt, _) = client_crypt(&find_crypt_setup(&messages));

    // Before any UDP traffic the server has decrypted nothing, and says so.
    send(
        &mut writer,
        &ControlMessage::Ping(tcp::Ping {
            timestamp: Some(1),
            ..Default::default()
        }),
    )
    .await;
    let first = next_ping(&mut reader).await;
    assert_eq!(first.timestamp, Some(1), "timestamp echoed");
    assert_eq!(first.good, Some(0), "no datagram decrypted yet");

    // One UDP ping, encrypted: the server decrypts it and replies.
    let sealed = crypt
        .encrypt(&encode_udp(&UdpMessage::Ping(udp::Ping {
            timestamp: 7,
            ..Default::default()
        })))
        .expect("encrypt udp ping");
    let sock = UdpSocket::bind("127.0.0.1:0").await.expect("bind udp");
    sock.send_to(&sealed, udp_addr).await.expect("send udp");
    let mut buf = vec![0u8; 2048];
    let (len, _from) = tokio::time::timeout(Duration::from_secs(5), sock.recv_from(&mut buf))
        .await
        .expect("no UDP reply before timeout")
        .expect("recv");
    crypt.decrypt(&buf[..len]).expect("decrypt udp ping reply");

    // The next TCP ping must report that datagram.
    send(
        &mut writer,
        &ControlMessage::Ping(tcp::Ping {
            timestamp: Some(2),
            ..Default::default()
        }),
    )
    .await;
    let second = next_ping(&mut reader).await;
    assert_eq!(second.timestamp, Some(2), "timestamp echoed");
    assert_eq!(second.good, Some(1), "the decrypted datagram is counted");
    assert_eq!(second.late, Some(0));
    assert_eq!(second.lost, Some(0));
    assert_eq!(second.resync, Some(0), "no resync is implemented in P3");

    handle.shutdown();
}

/// Read frames until a `Ping` arrives.
async fn next_ping(reader: &mut FramedReader) -> tcp::Ping {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), reader.next()).await {
            Ok(Some(ControlMessage::Ping(ping))) => return ping,
            Ok(Some(_)) => continue,
            _ => panic!("no Ping reply before timeout"),
        }
    }
}

#[tokio::test]
async fn tcp_tunnel_loopback_fallback() {
    let handle = start_server().await;
    let (mut reader, mut writer) = connect(&handle).await;
    let messages = do_handshake(&mut reader, &mut writer, "carol").await;
    let my_session = session_of(&messages);

    // Tunnel a loopback audio packet over TCP (UDPTunnel carries the plaintext
    // UDP packet). The server reflects it back as a UDPTunnel.
    let opus = vec![0x01u8, 0x02, 0x03];
    let audio = udp::Audio {
        header: Some(udp::audio::Header::Target(LOOPBACK_TARGET)),
        frame_number: 7,
        opus_data: opus.clone(),
        ..Default::default()
    };
    let tunneled = encode_udp(&UdpMessage::Audio(audio));
    send(&mut writer, &ControlMessage::UdpTunnel(tunneled)).await;

    // Read until we get the reflected tunnel packet.
    let reflected = loop {
        match tokio::time::timeout(Duration::from_secs(5), reader.next()).await {
            Ok(Some(ControlMessage::UdpTunnel(bytes))) => break bytes,
            Ok(Some(_)) => continue,
            _ => panic!("no tunnelled reply before timeout"),
        }
    };
    match decode_udp(&reflected).expect("decode tunnel reply") {
        UdpMessage::Audio(audio) => {
            assert_eq!(audio.sender_session, my_session);
            assert_eq!(audio.opus_data, opus);
        }
        other => panic!("expected Audio in tunnel, got {other:?}"),
    }

    handle.shutdown();
}

// ---------------------------------------------------------------------------
// Phase 4: routing between two clients, on both transports.
// ---------------------------------------------------------------------------

const NORMAL_TARGET: u32 = 0;

/// One connected test client: its control stream, its OCB2 state, its session
/// and a UDP socket of its own.
struct Client {
    reader: FramedReader,
    writer: WriteHalf<TlsStream<TcpStream>>,
    crypt: CryptState,
    session: u32,
    sock: UdpSocket,
}

/// Connect, authenticate and take the crypto material.
async fn join(handle: &ServerHandle, name: &str) -> Client {
    let (mut reader, mut writer) = connect(handle).await;
    let messages = do_handshake(&mut reader, &mut writer, name).await;
    let (crypt, _) = client_crypt(&find_crypt_setup(&messages));
    Client {
        reader,
        writer,
        crypt,
        session: session_of(&messages),
        sock: UdpSocket::bind("127.0.0.1:0").await.expect("bind udp"),
    }
}

/// Prove ownership of the UDP address by pinging, so the server binds it. The
/// reply also confirms the association actually happened.
async fn associate_udp(client: &mut Client, udp_addr: std::net::SocketAddr) {
    let sealed = client
        .crypt
        .encrypt(&encode_udp(&UdpMessage::Ping(udp::Ping {
            timestamp: 1,
            ..Default::default()
        })))
        .expect("encrypt ping");
    client.sock.send_to(&sealed, udp_addr).await.expect("send");

    let mut buf = vec![0u8; 2048];
    let (len, _from) =
        tokio::time::timeout(Duration::from_secs(5), client.sock.recv_from(&mut buf))
            .await
            .expect("no UDP ping reply before timeout")
            .expect("recv");
    client
        .crypt
        .decrypt(&buf[..len])
        .expect("decrypt ping reply");
}

fn speech(target: u32, opus: &[u8]) -> Vec<u8> {
    encode_udp(&UdpMessage::Audio(udp::Audio {
        header: Some(udp::audio::Header::Target(target)),
        frame_number: 11,
        opus_data: opus.to_vec(),
        ..Default::default()
    }))
}

/// Receive one datagram and decrypt it as audio.
async fn recv_audio(client: &mut Client) -> udp::Audio {
    let mut buf = vec![0u8; 2048];
    let (len, _from) =
        tokio::time::timeout(Duration::from_secs(5), client.sock.recv_from(&mut buf))
            .await
            .expect("no UDP audio before timeout")
            .expect("recv");
    let plain = client.crypt.decrypt(&buf[..len]).expect("decrypt audio");
    match decode_udp(&plain).expect("decode audio") {
        UdpMessage::Audio(audio) => audio,
        other => panic!("expected Audio, got {other:?}"),
    }
}

/// Read control frames until a tunnelled audio packet arrives.
async fn recv_tunnelled_audio(client: &mut Client) -> udp::Audio {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), client.reader.next()).await {
            Ok(Some(ControlMessage::UdpTunnel(bytes))) => {
                match decode_udp(&bytes).expect("decode tunnelled audio") {
                    UdpMessage::Audio(audio) => return audio,
                    other => panic!("expected Audio in tunnel, got {other:?}"),
                }
            }
            Ok(Some(_)) => continue,
            _ => panic!("no tunnelled audio before timeout"),
        }
    }
}

/// Normal speech (target 0) reaches the other connected client, carries the
/// authenticated sender session and a context header, and never comes back to
/// its own sender.
#[tokio::test]
async fn two_clients_hear_each_other_over_udp() {
    let handle = start_server().await;
    let udp_addr = handle.udp_addr;
    let mut alice = join(&handle, "alice").await;
    let mut bob = join(&handle, "bob").await;
    associate_udp(&mut alice, udp_addr).await;
    associate_udp(&mut bob, udp_addr).await;

    let opus = vec![0x11u8, 0x22, 0x33, 0x44];
    let sealed = alice
        .crypt
        .encrypt(&speech(NORMAL_TARGET, &opus))
        .expect("encrypt");
    alice.sock.send_to(&sealed, udp_addr).await.expect("send");

    let heard = recv_audio(&mut bob).await;
    assert_eq!(heard.opus_data, opus, "Opus survives the relay");
    assert_eq!(
        heard.sender_session, alice.session,
        "the packet is attributed to the authenticated sender"
    );
    assert_eq!(
        heard.header,
        Some(udp::audio::Header::Context(0)),
        "server-to-client audio carries context, not target"
    );

    // Bob already has it, so anything destined for Alice would have been sent
    // before that; her socket must be empty.
    let mut buf = vec![0u8; 2048];
    let echo =
        tokio::time::timeout(Duration::from_millis(250), alice.sock.recv_from(&mut buf)).await;
    assert!(echo.is_err(), "normal speech must not echo to its sender");

    handle.shutdown();
}

/// A client whose UDP does not work speaks through the tunnel; a listener on UDP
/// still hears it. This is the cross-transport half of spec 15.6.
#[tokio::test]
async fn tunnelled_speech_reaches_a_udp_listener() {
    let handle = start_server().await;
    let udp_addr = handle.udp_addr;
    let mut alice = join(&handle, "alice").await;
    let mut bob = join(&handle, "bob").await;
    associate_udp(&mut bob, udp_addr).await;

    let opus = vec![0x55u8, 0x66];
    send(
        &mut alice.writer,
        &ControlMessage::UdpTunnel(speech(NORMAL_TARGET, &opus)),
    )
    .await;

    let heard = recv_audio(&mut bob).await;
    assert_eq!(heard.opus_data, opus);
    assert_eq!(heard.sender_session, alice.session);

    handle.shutdown();
}

/// The mirror case: the speaker is on UDP, the listener fell back to the tunnel.
/// Tunnelling audio is what tells the server the listener's UDP is unusable.
#[tokio::test]
async fn udp_speech_reaches_a_tunnelled_listener() {
    let handle = start_server().await;
    let udp_addr = handle.udp_addr;
    let mut alice = join(&handle, "alice").await;
    let mut bob = join(&handle, "bob").await;
    associate_udp(&mut alice, udp_addr).await;
    associate_udp(&mut bob, udp_addr).await;

    // Bob tunnels a packet, which marks him as TCP-bound from now on.
    send(
        &mut bob.writer,
        &ControlMessage::UdpTunnel(speech(NORMAL_TARGET, &[0x01])),
    )
    .await;
    // Alice hears that one over UDP, which also proves the server processed it
    // and therefore recorded Bob's transport before we speak to him.
    let _first = recv_audio(&mut alice).await;

    let opus = vec![0x77u8, 0x88, 0x99];
    let sealed = alice
        .crypt
        .encrypt(&speech(NORMAL_TARGET, &opus))
        .expect("encrypt");
    alice.sock.send_to(&sealed, udp_addr).await.expect("send");

    let heard = recv_tunnelled_audio(&mut bob).await;
    assert_eq!(heard.opus_data, opus);
    assert_eq!(heard.sender_session, alice.session);

    handle.shutdown();
}

/// Shout and whisper targets are registered with `VoiceTarget`, which the server
/// refuses. Routing one as if it were normal speech would deliver voice the
/// client never asked to send to those listeners.
#[tokio::test]
async fn an_unregistered_voice_target_is_not_routed() {
    let handle = start_server().await;
    let udp_addr = handle.udp_addr;
    let mut alice = join(&handle, "alice").await;
    let mut bob = join(&handle, "bob").await;
    associate_udp(&mut alice, udp_addr).await;
    associate_udp(&mut bob, udp_addr).await;

    let sealed = alice.crypt.encrypt(&speech(7, &[0xAB])).expect("encrypt");
    alice.sock.send_to(&sealed, udp_addr).await.expect("send");

    let mut buf = vec![0u8; 2048];
    let delivered =
        tokio::time::timeout(Duration::from_millis(250), bob.sock.recv_from(&mut buf)).await;
    assert!(delivered.is_err(), "target 7 must not be routed");

    // The plane is still alive: normal speech right after still gets through.
    let sealed = alice
        .crypt
        .encrypt(&speech(NORMAL_TARGET, &[0xCD]))
        .expect("encrypt");
    alice.sock.send_to(&sealed, udp_addr).await.expect("send");
    let heard = recv_audio(&mut bob).await;
    assert_eq!(heard.opus_data, vec![0xCD]);

    handle.shutdown();
}

/// Minimal framed reader over a TLS client stream.
struct FramedReader {
    read: ReadHalf<TlsStream<TcpStream>>,
    buffer: Vec<u8>,
}

impl FramedReader {
    fn new(read: ReadHalf<TlsStream<TcpStream>>) -> Self {
        Self {
            read,
            buffer: Vec::new(),
        }
    }

    /// Next control message, or `None` on clean EOF.
    async fn next(&mut self) -> Option<ControlMessage> {
        loop {
            if let Some(frame) = parse_frame(&self.buffer).expect("parse frame") {
                let consumed = frame.total_len();
                let message = decode_frame(&frame).expect("decode frame");
                self.buffer.drain(..consumed);
                return Some(message);
            }
            let mut chunk = [0u8; 4096];
            let read = self.read.read(&mut chunk).await.expect("read");
            if read == 0 {
                return None;
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }
}

/// A verifier that trusts any server certificate (local test only).
#[derive(Debug)]
struct TrustAnyServer {
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for TrustAnyServer {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}
