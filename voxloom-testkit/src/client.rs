//! The async `SimulatedMumbleClient`: a real TLS Mumble client whose only job is
//! to judge the server. It connects, authenticates, and feeds every received
//! control message through [`ClientModel`], which panics on any §20 violation.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use voxloom_protocol::messages::tcp;
use voxloom_protocol::{ControlMessage, decode_frame, encode_frame, parse_frame};

use crate::model::ClientModel;
use crate::tls;

/// How long the client waits for the handshake to complete before giving up.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// A strict simulated Mumble client driven over a real TLS connection.
pub struct SimulatedMumbleClient {
    reader: FrameReader,
    writer: WriteHalf<TlsStream<TcpStream>>,
    model: ClientModel,
}

impl SimulatedMumbleClient {
    /// Connect to a server, send `Version` and `Authenticate`, but do not yet
    /// read the handshake — call [`SimulatedMumbleClient::drive_handshake`].
    pub async fn connect(server: SocketAddr, username: &str) -> Result<Self> {
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

        let mut client = Self {
            reader: FrameReader::new(read),
            writer: write,
            model: ClientModel::new(),
        };

        client
            .send(&ControlMessage::Version(tcp::Version {
                release: Some("voxloom-testkit".to_string()),
                // Advertise 1.5.0 so the server treats us as a protobuf-UDP client.
                version_v2: Some((1u64 << 48) | (5u64 << 32)),
                ..Default::default()
            }))
            .await?;
        client
            .send(&ControlMessage::Authenticate(tcp::Authenticate {
                username: Some(username.to_string()),
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
        while !self.model.synced {
            match tokio::time::timeout(HANDSHAKE_TIMEOUT, self.reader.next()).await {
                Ok(Ok(Some(message))) => self.model.apply(&message),
                Ok(Ok(None)) => anyhow::bail!("server closed the connection before ServerSync"),
                Ok(Err(error)) => return Err(error),
                Err(_) => anyhow::bail!("timed out waiting for ServerSync"),
            }
        }
        Ok(())
    }

    /// Drain and apply any messages that arrive within `window` (e.g. presence
    /// updates broadcast after the handshake). Returns when the window elapses
    /// with no further message.
    pub async fn pump(&mut self, window: Duration) -> Result<()> {
        loop {
            match tokio::time::timeout(window, self.reader.next()).await {
                Ok(Ok(Some(message))) => self.model.apply(&message),
                Ok(Ok(None)) => return Ok(()), // connection closed
                Ok(Err(error)) => return Err(error),
                Err(_) => return Ok(()), // idle: nothing more within the window
            }
        }
    }

    /// The current model view.
    pub fn model(&self) -> &ClientModel {
        &self.model
    }

    /// This connection's own session id, once synced.
    pub fn self_session(&self) -> Option<u32> {
        self.model.self_session
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
