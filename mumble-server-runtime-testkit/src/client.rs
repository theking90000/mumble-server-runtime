//! The async `SimulatedMumbleClient`: a real TLS Mumble client whose only job is
//! to judge the server. It connects, authenticates, and feeds every received
//! control message through [`ClientModel`], which panics on any §20 violation.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result};
use mumble_server_runtime_crypto::{BLOCK_SIZE, CryptState, KEY_SIZE};
use mumble_server_runtime_protocol::messages::{tcp, udp};
use mumble_server_runtime_protocol::{
    ControlMessage, UdpMessage, decode_frame, decode_udp, encode_frame, encode_udp, parse_frame,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{TcpStream, UdpSocket};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::model::ClientModel;
use crate::tls;

/// How long the client waits for the handshake to complete before giving up.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Largest datagram the judge will read. Anything bigger than the protocol's own
/// limit is a server defect, and truncating here would hide it.
const VOICE_BUFFER: usize = 2048;

/// A strict simulated Mumble client driven over a real TLS connection.
///
/// It also owns a voice plane, because judging the audio data plane needs the
/// same connection's OCB2 material: the key arrives on the control channel and
/// is what proves ownership of a UDP address.
pub struct SimulatedMumbleClient {
    reader: FrameReader,
    writer: WriteHalf<TlsStream<TcpStream>>,
    model: ClientModel,
    /// Derived from the server's `CryptSetup`. `None` until it arrives.
    crypt: Option<CryptState>,
    voice: UdpSocket,
}

impl SimulatedMumbleClient {
    /// Connect to a server, send `Version` and `Authenticate`, but do not yet
    /// read the handshake — call [`SimulatedMumbleClient::drive_handshake`].
    pub async fn connect(server: SocketAddr, username: &str) -> Result<Self> {
        Self::connect_inner(server, username, None).await
    }

    /// Connect with an opaque credential in `Authenticate.password`.
    ///
    /// REF: `references/mumble/src/Mumble.proto:Authenticate.password` defines
    /// the password field used by Mumble clients for server authentication.
    pub async fn connect_with_credential(
        server: SocketAddr,
        username: &str,
        credential: &str,
    ) -> Result<Self> {
        Self::connect_inner(server, username, Some(credential)).await
    }

    async fn connect_inner(
        server: SocketAddr,
        username: &str,
        credential: Option<&str>,
    ) -> Result<Self> {
        tls::install_crypto_provider();
        let tcp = TcpStream::connect(server)
            .await
            .with_context(|| format!("connecting to {server}"))?;
        let _ignored = tcp.set_nodelay(true);
        let connector = TlsConnector::from(tls::client_config());
        let name = rustls::pki_types::ServerName::try_from("localhost").context("server name")?;
        let stream = connector
            .connect(name, tcp)
            .await
            .context("TLS handshake")?;
        let (read, write) = tokio::io::split(stream);

        let voice = UdpSocket::bind("127.0.0.1:0")
            .await
            .context("binding the judge's voice socket")?;

        let mut client = Self {
            reader: FrameReader::new(read),
            writer: write,
            model: ClientModel::new(),
            crypt: None,
            voice,
        };

        client
            .send(&ControlMessage::Version(tcp::Version {
                release: Some("mumble-server-runtime-testkit".to_string()),
                // Advertise 1.5.0 so the server treats us as a protobuf-UDP client.
                version_v2: Some((1u64 << 48) | (5u64 << 32)),
                ..Default::default()
            }))
            .await?;
        client
            .send(&ControlMessage::Authenticate(tcp::Authenticate {
                username: Some(username.to_string()),
                password: credential.map(str::to_owned),
                opus: Some(true),
                ..Default::default()
            }))
            .await?;
        Ok(client)
    }

    /// Read and apply messages until `ServerSync` arrives. Every message is
    /// validated by the model, which panics on a §20 violation. Errors if the
    /// connection closes or times out before sync.
    pub async fn drive_handshake(&mut self) -> Result<()> {
        self.wait_until(HANDSHAKE_TIMEOUT, |model| model.synced)
            .await
            .context("waiting for ServerSync")
    }

    /// Apply control messages until `predicate` accepts the strict local model.
    ///
    /// The duration is only a failure deadline. Progress is synchronized by the
    /// requested model state, never by assuming that an idle socket is settled.
    pub async fn wait_until<Predicate>(
        &mut self,
        deadline: Duration,
        predicate: Predicate,
    ) -> Result<()>
    where
        Predicate: Fn(&ClientModel) -> bool,
    {
        if predicate(&self.model) {
            return Ok(());
        }
        tokio::time::timeout(deadline, async {
            loop {
                let message = self
                    .reader
                    .next()
                    .await?
                    .context("server closed the control connection before the expected view")?;
                self.observe(&message);
                self.model.apply(&message);
                if predicate(&self.model) {
                    return Ok(());
                }
            }
        })
        .await
        .context("timed out waiting for the expected client model")?
    }

    /// The current model view.
    pub fn model(&self) -> &ClientModel {
        &self.model
    }

    /// This connection's own session id, once synced.
    pub fn self_session(&self) -> Option<u32> {
        self.model.self_session
    }

    /// Capture what the judge needs from a message before the model validates it.
    ///
    /// The model deliberately knows nothing about crypto (it enforces §20
    /// structure), so the key is taken here instead of widening the model.
    fn observe(&mut self, message: &ControlMessage) {
        if let ControlMessage::CryptSetup(setup) = message {
            self.crypt = client_crypt(setup);
        }
    }

    /// Prove ownership of this client's UDP address by sending an encrypted
    /// ping and waiting for the reply.
    ///
    /// Association is by cryptographic proof, so a client that never sends a
    /// datagram simply has no UDP address as far as the server is concerned;
    /// the judge must do this before expecting voice over UDP.
    pub async fn associate_udp(&mut self, voice_addr: SocketAddr, timeout: Duration) -> Result<()> {
        let ping = encode_udp(&UdpMessage::Ping(udp::Ping {
            timestamp: 1,
            ..Default::default()
        }));
        self.send_voice(voice_addr, &ping).await?;

        let mut buffer = vec![0u8; VOICE_BUFFER];
        let received = tokio::time::timeout(timeout, self.voice.recv_from(&mut buffer))
            .await
            .context("timed out waiting for the UDP ping reply")?;
        let (len, _from) = received.context("receiving the UDP ping reply")?;

        let crypt = self
            .crypt
            .as_mut()
            .context("no CryptSetup received, cannot decrypt")?;
        let plaintext = crypt
            .decrypt(buffer.get(..len).unwrap_or(&[]))
            .context("the UDP ping reply did not authenticate")?;
        match decode_udp(&plaintext).context("decoding the UDP ping reply")? {
            UdpMessage::Ping(_) => Ok(()),
            other => anyhow::bail!("expected a Ping reply, got {other:?}"),
        }
    }

    /// Send one voice packet to the given target.
    pub async fn speak(
        &mut self,
        voice_addr: SocketAddr,
        target: u32,
        frame_number: u64,
        opus: &[u8],
    ) -> Result<()> {
        let packet = encode_udp(&UdpMessage::Audio(udp::Audio {
            header: Some(udp::audio::Header::Target(target)),
            frame_number,
            opus_data: opus.to_vec(),
            ..Default::default()
        }));
        self.send_voice(voice_addr, &packet).await
    }

    /// Receive one voice packet, or `None` if none arrives within `timeout`.
    ///
    /// A datagram that arrives but fails to authenticate or decode is an error,
    /// not a `None`: silence and corruption are different verdicts and the judge
    /// must not blur them.
    pub async fn recv_voice(&mut self, timeout: Duration) -> Result<Option<udp::Audio>> {
        let mut buffer = vec![0u8; VOICE_BUFFER];
        let Ok(received) = tokio::time::timeout(timeout, self.voice.recv_from(&mut buffer)).await
        else {
            return Ok(None);
        };
        let (len, _from) = received.context("receiving voice")?;

        let crypt = self
            .crypt
            .as_mut()
            .context("no CryptSetup received, cannot decrypt")?;
        let plaintext = crypt
            .decrypt(buffer.get(..len).unwrap_or(&[]))
            .context("a voice datagram did not authenticate")?;
        match decode_udp(&plaintext).context("decoding voice")? {
            UdpMessage::Audio(audio) => {
                // REF: references/mumble/src/MumbleUDP.proto:Audio.sender_session
                self.model.apply_audio(&audio);
                Ok(Some(audio))
            }
            UdpMessage::Ping(_) => Ok(None),
        }
    }

    async fn send_voice(&mut self, voice_addr: SocketAddr, plaintext: &[u8]) -> Result<()> {
        let crypt = self
            .crypt
            .as_mut()
            .context("no CryptSetup received, cannot encrypt")?;
        let sealed = crypt.encrypt(plaintext).context("OCB2 encrypt failed")?;
        self.voice
            .send_to(&sealed, voice_addr)
            .await
            .context("sending a voice datagram")?;
        Ok(())
    }

    /// The judge's own outbound control channel, for driving the server.
    pub async fn send_control(&mut self, message: &ControlMessage) -> Result<()> {
        self.send(message).await
    }

    async fn send(&mut self, message: &ControlMessage) -> Result<()> {
        let mut framed = Vec::new();
        encode_frame(message, &mut framed).context("encoding frame")?;
        self.writer.write_all(&framed).await.context("TLS write")?;
        self.writer.flush().await.context("TLS flush")?;
        Ok(())
    }
}

/// Incremental frame reader over the TLS read half.
struct FrameReader {
    read: ReadHalf<TlsStream<TcpStream>>,
    buffer: Vec<u8>,
}

impl FrameReader {
    fn new(read: ReadHalf<TlsStream<TcpStream>>) -> Self {
        Self {
            read,
            buffer: Vec::with_capacity(4096),
        }
    }

    /// Next control message, or `None` at a clean EOF on a frame boundary.
    async fn next(&mut self) -> Result<Option<ControlMessage>> {
        loop {
            if let Some((message, consumed)) = self.try_parse()? {
                self.buffer.drain(..consumed);
                return Ok(Some(message));
            }
            let mut chunk = [0u8; 4096];
            let read = self.read.read(&mut chunk).await.context("TLS read")?;
            if read == 0 {
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                anyhow::bail!("connection closed mid-frame");
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }

    fn try_parse(&self) -> Result<Option<(ControlMessage, usize)>> {
        match parse_frame(&self.buffer).context("framing")? {
            Some(frame) => {
                let consumed = frame.total_len();
                let message = decode_frame(&frame).context("decoding control message")?;
                Ok(Some((message, consumed)))
            }
            None => Ok(None),
        }
    }
}

/// Build the client side of the OCB2 state from the server's `CryptSetup`.
///
/// Returns `None` for a malformed setup rather than guessing: a judge that
/// invents crypto material would report the server's mistakes as its own.
///
/// REF: references/mumble/src/mumble/Messages.cpp : `setKey(key, client_nonce,
///   server_nonce)` — the client encrypts client-to-server with the client
///   nonce and decrypts server-to-client with the server nonce.
fn client_crypt(setup: &tcp::CryptSetup) -> Option<CryptState> {
    let key: [u8; KEY_SIZE] = setup.key.as_deref()?.try_into().ok()?;
    let client_nonce: [u8; BLOCK_SIZE] = setup.client_nonce.as_deref()?.try_into().ok()?;
    let server_nonce: [u8; BLOCK_SIZE] = setup.server_nonce.as_deref()?.try_into().ok()?;
    Some(CryptState::new(&key, &client_nonce, &server_nonce))
}
