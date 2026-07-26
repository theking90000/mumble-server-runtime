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
