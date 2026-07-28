//! A minimal Mumble client, just enough to exercise the gateway end to end.
//!
//! Deliberately its own thing rather than a borrowed one: the strict simulated
//! client is a verifier-zone deliverable and R2 keeps it out of an
//! implementation diff. This model judges nothing. It reads frames, tracks the
//! tree, and can speak - so a test can assert on what a real client would hold
//! without the model itself deciding what "correct" means.
#![allow(clippy::expect_used)]
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error as TlsError, SignatureScheme};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{TcpStream, UdpSocket};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use mumble_server_runtime_crypto::{BLOCK_SIZE, CryptState, KEY_SIZE};
use mumble_server_runtime_protocol::messages::{tcp, udp};
use mumble_server_runtime_protocol::{
    ControlMessage, UdpMessage, decode_frame, decode_udp, encode_frame, encode_udp, parse_frame,
};

/// Accepts any server certificate.
///
/// The gateway generates a fresh self-signed one per run, and what these tests
/// exercise is the Mumble protocol above TLS, not a trust decision.
#[derive(Debug)]
struct AcceptAnyServer;

impl ServerCertVerifier for AcceptAnyServer {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _certificate: &CertificateDer<'_>,
        _signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _certificate: &CertificateDer<'_>,
        _signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// What the client believes the world looks like.
#[derive(Debug, Default)]
pub struct Model {
    pub channels: BTreeMap<u32, Channel>,
    pub users: BTreeMap<u32, User>,
    /// Set by `ServerSync`.
    pub session: Option<u32>,
    /// Every `UserRemove` seen, in order. Kept because the interesting property
    /// is often that one did **not** arrive.
    pub removed_users: Vec<u32>,
    pub removed_channels: Vec<u32>,
    /// Tunnelled voice packets, still sealed in their envelope.
    pub tunnelled: Vec<udp::Audio>,
    /// Effective permissions, per channel, as the server answered them.
    pub permissions: BTreeMap<u32, u32>,
    /// Every `UserStats` answer, in order.
    pub stats: Vec<tcp::UserStats>,
    /// The context-action menu, as the client would build it: identifier to
    /// label. An `Add` inserts, a `Remove` deletes.
    pub actions: BTreeMap<String, String>,
    /// Every text message received, in order.
    pub said: Vec<tcp::TextMessage>,
    /// Every refusal received, in order.
    pub refused: Vec<tcp::PermissionDenied>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    pub name: String,
    pub parent: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct User {
    pub name: String,
    pub channel: u32,
    pub self_mute: bool,
    pub self_deaf: bool,
}

impl Model {
    pub fn channel_named(&self, name: &str) -> Option<u32> {
        self.channels
            .iter()
            .find(|(_, channel)| channel.name == name)
            .map(|(id, _)| *id)
    }

    pub fn user_named(&self, name: &str) -> Option<u32> {
        self.users
            .iter()
            .find(|(_, user)| user.name == name)
            .map(|(session, _)| *session)
    }

    pub fn channel_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .channels
            .values()
            .map(|channel| channel.name.clone())
            .collect();
        names.sort();
        names
    }

    pub fn user_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.users.values().map(|user| user.name.clone()).collect();
        names.sort();
        names
    }

    fn apply(&mut self, message: &ControlMessage) {
        match message {
            ControlMessage::ChannelState(state) => {
                let Some(id) = state.channel_id else { return };
                let entry = self.channels.entry(id).or_insert(Channel {
                    name: String::new(),
                    parent: state.parent,
                });
                if let Some(name) = &state.name {
                    entry.name.clone_from(name);
                }
                if state.parent.is_some() {
                    entry.parent = state.parent;
                }
            }
            ControlMessage::ChannelRemove(remove) => {
                self.channels.remove(&remove.channel_id);
                self.removed_channels.push(remove.channel_id);
            }
            ControlMessage::UserState(state) => {
                let Some(session) = state.session else { return };
                let entry = self.users.entry(session).or_default();
                if let Some(name) = &state.name {
                    entry.name.clone_from(name);
                }
                if let Some(channel) = state.channel_id {
                    entry.channel = channel;
                }
                // Sparse, like the real client's model: a field the server left
                // out is a field nobody touched.
                if let Some(mute) = state.self_mute {
                    entry.self_mute = mute;
                }
                if let Some(deaf) = state.self_deaf {
                    entry.self_deaf = deaf;
                }
            }
            ControlMessage::UserRemove(remove) => {
                self.users.remove(&remove.session);
                self.removed_users.push(remove.session);
            }
            ControlMessage::ServerSync(sync) => self.session = sync.session,
            ControlMessage::PermissionQuery(query) => {
                if let (Some(channel), Some(permissions)) = (query.channel_id, query.permissions) {
                    self.permissions.insert(channel, permissions);
                }
            }
            ControlMessage::UserStats(stats) => self.stats.push(stats.clone()),
            ControlMessage::ContextActionModify(modify) => {
                let remove = modify.operation
                    == Some(i32::from(tcp::context_action_modify::Operation::Remove));
                if remove {
                    self.actions.remove(&modify.action);
                } else {
                    self.actions.insert(
                        modify.action.clone(),
                        modify.text.clone().unwrap_or_default(),
                    );
                }
            }
            ControlMessage::TextMessage(text) => self.said.push(text.clone()),
            ControlMessage::PermissionDenied(denied) => self.refused.push(denied.clone()),
            ControlMessage::UdpTunnel(raw) => {
                if let Ok(UdpMessage::Audio(audio)) = decode_udp(raw) {
                    self.tunnelled.push(audio);
                }
            }
            _ => {}
        }
    }
}

/// A connected client.
pub struct Client {
    reader: ReadHalf<TlsStream<TcpStream>>,
    writer: WriteHalf<TlsStream<TcpStream>>,
    buffer: Vec<u8>,
    pub model: Model,
    crypt: Option<CryptState>,
    server: SocketAddr,
    udp: Option<Arc<UdpSocket>>,
}

impl Client {
    /// Connect, authenticate, and read up to and including `ServerSync`.
    pub async fn connect(
        server: SocketAddr,
        name: &str,
        credential: Option<&str>,
    ) -> Result<Client> {
        let _ignored = rustls::crypto::ring::default_provider().install_default();
        let config =
            rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS12])
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AcceptAnyServer))
                .with_no_client_auth();

        let tcp = TcpStream::connect(server).await.context("connect")?;
        let stream = TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from("localhost")?, tcp)
            .await
            .context("TLS")?;
        let (reader, writer) = tokio::io::split(stream);

        let mut client = Client {
            reader,
            writer,
            buffer: Vec::with_capacity(4096),
            model: Model::default(),
            crypt: None,
            server,
            udp: None,
        };

        client
            .send(&ControlMessage::Version(tcp::Version {
                version_v2: Some(1 << 48 | 5 << 32),
                release: Some("test-client".to_owned()),
                ..Default::default()
            }))
            .await?;
        client
            .send(&ControlMessage::Authenticate(tcp::Authenticate {
                username: Some(name.to_owned()),
                password: credential.map(str::to_owned),
                opus: Some(true),
                ..Default::default()
            }))
            .await?;

        // Read until the server has synchronised us.
        loop {
            let message = client.receive().await?;
            if matches!(message, ControlMessage::ServerSync(_)) {
                return Ok(client);
            }
        }
    }

    pub fn session(&self) -> u32 {
        self.model.session.unwrap_or_default()
    }

    pub async fn send(&mut self, message: &ControlMessage) -> Result<()> {
        let mut framed = Vec::new();
        encode_frame(message, &mut framed)?;
        self.writer.write_all(&framed).await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// Ask to enter a channel, the way a double-click does.
    pub async fn enter(&mut self, channel: u32) -> Result<()> {
        let session = self.session();
        self.send(&ControlMessage::UserState(tcp::UserState {
            session: Some(session),
            channel_id: Some(channel),
            ..Default::default()
        }))
        .await
    }

    /// Mute or deafen itself, the way the mute button does.
    ///
    /// No session field, deliberately: that is exactly what the official client
    /// sends, and it is the case a server that only accepts an explicit session
    /// would silently ignore.
    ///
    /// REF: references/mumble/src/mumble/ServerHandler.cpp :
    ///   `setSelfMuteDeafState`.
    pub async fn set_self_state(&mut self, mute: bool, deaf: bool) -> Result<()> {
        self.send(&ControlMessage::UserState(tcp::UserState {
            self_mute: Some(mute),
            self_deaf: Some(deaf),
            ..Default::default()
        }))
        .await
    }

    /// Type into the chat bar with a channel selected.
    ///
    /// REF: references/mumble/src/mumble/ServerHandler.cpp :
    ///   `sendChannelTextMessage` fills one `channel_id`, or one `tree_id` when
    ///   the message is aimed at the whole subtree.
    pub async fn say_in_channel(&mut self, channel: u32, message: &str) -> Result<()> {
        self.send(&ControlMessage::TextMessage(tcp::TextMessage {
            channel_id: vec![channel],
            message: message.to_owned(),
            ..Default::default()
        }))
        .await
    }

    /// Write privately to somebody, the way the user menu does.
    ///
    /// REF: references/mumble/src/mumble/ServerHandler.cpp :
    ///   `sendUserTextMessage`.
    pub async fn say_to_user(&mut self, session: u32, message: &str) -> Result<()> {
        self.send(&ControlMessage::TextMessage(tcp::TextMessage {
            session: vec![session],
            message: message.to_owned(),
            ..Default::default()
        }))
        .await
    }

    /// Ask what may be done in a channel, the way selecting it does.
    ///
    /// REF: references/mumble/src/mumble/ServerHandler.cpp :
    ///   `requestChannelPermissions`.
    pub async fn query_permissions(&mut self, channel: u32) -> Result<()> {
        self.send(&ControlMessage::PermissionQuery(tcp::PermissionQuery {
            channel_id: Some(channel),
            ..Default::default()
        }))
        .await
    }

    /// Open somebody's information window.
    ///
    /// REF: references/mumble/src/mumble/ServerHandler.cpp : `requestUserStats`.
    /// Press a context action, the way the client does: the identifier it was
    /// given, plus whatever the tree currently has selected.
    pub async fn invoke_action(
        &mut self,
        action: &str,
        session: Option<u32>,
        channel: Option<u32>,
    ) -> Result<()> {
        self.send(&ControlMessage::ContextAction(tcp::ContextAction {
            action: action.to_owned(),
            session,
            channel_id: channel,
        }))
        .await
    }

    pub async fn request_user_stats(&mut self, session: u32) -> Result<()> {
        self.send(&ControlMessage::UserStats(tcp::UserStats {
            session: Some(session),
            stats_only: Some(false),
            ..Default::default()
        }))
        .await
    }

    /// Report what this client sees of the link, the way its keepalive does.
    ///
    /// REF: references/mumble/src/mumble/ServerHandler.cpp : the client's `Ping`
    ///   carries its own good/late/lost counters, packet counts and measured
    ///   pings.
    pub async fn ping_reporting(&mut self, report: tcp::Ping) -> Result<()> {
        self.send(&ControlMessage::Ping(report)).await
    }

    /// Read one message, applying it to the model.
    pub async fn receive(&mut self) -> Result<ControlMessage> {
        loop {
            if let Some(frame) = parse_frame(&self.buffer)? {
                let consumed = frame.total_len();
                let message = decode_frame(&frame)?;
                self.buffer.drain(..consumed);
                self.model.apply(&message);
                if let ControlMessage::CryptSetup(setup) = &message {
                    self.adopt(setup);
                }
                return Ok(message);
            }

            let mut chunk = [0u8; 4096];
            let read = self.reader.read(&mut chunk).await?;
            anyhow::ensure!(read > 0, "the server closed the connection");
            self.buffer
                .extend_from_slice(chunk.get(..read).unwrap_or_default());
        }
    }

    /// Read messages until `predicate` holds on the model, or the deadline
    /// passes.
    ///
    /// Reading rather than sleeping: the test advances when the server has
    /// actually said something, so there is no timing to tune.
    pub async fn settle(&mut self, label: &str, predicate: impl Fn(&Model) -> bool) -> Result<()> {
        if predicate(&self.model) {
            return Ok(());
        }
        let deadline = std::time::Duration::from_secs(5);
        tokio::time::timeout(deadline, async {
            loop {
                self.receive().await?;
                if predicate(&self.model) {
                    return Ok::<(), anyhow::Error>(());
                }
            }
        })
        .await
        .with_context(|| format!("timed out waiting for {label}"))?
    }

    fn adopt(&mut self, setup: &tcp::CryptSetup) {
        let (Some(key), Some(client_nonce), Some(server_nonce)) =
            (&setup.key, &setup.client_nonce, &setup.server_nonce)
        else {
            return;
        };
        let (Ok(key), Ok(client_nonce), Ok(server_nonce)) = (
            <[u8; KEY_SIZE]>::try_from(key.as_slice()),
            <[u8; BLOCK_SIZE]>::try_from(client_nonce.as_slice()),
            <[u8; BLOCK_SIZE]>::try_from(server_nonce.as_slice()),
        ) else {
            return;
        };
        // Mirrored: what the server encrypts with, the client decrypts with.
        self.crypt = Some(CryptState::new(&key, &client_nonce, &server_nonce));
    }

    /// Bind a UDP socket and prove ownership of it by sending an encrypted ping.
    pub async fn open_udp(&mut self) -> Result<()> {
        let socket = UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
        socket.connect(self.server).await?;
        self.udp = Some(Arc::new(socket));
        self.send_udp(&UdpMessage::Ping(udp::Ping {
            timestamp: 1,
            ..Default::default()
        }))
        .await
    }

    pub async fn send_udp(&mut self, message: &UdpMessage) -> Result<()> {
        let socket = self.udp.clone().context("no UDP socket")?;
        let crypt = self.crypt.as_mut().context("no crypto state")?;
        let sealed = crypt.encrypt(&encode_udp(message)).context("encrypting")?;
        socket.send(&sealed).await?;
        Ok(())
    }

    /// Speak: one normal-target voice packet.
    pub async fn speak(&mut self, payload: &[u8]) -> Result<()> {
        self.send_udp(&UdpMessage::Audio(udp::Audio {
            header: Some(udp::audio::Header::Target(0)),
            frame_number: 1,
            opus_data: payload.to_vec(),
            ..Default::default()
        }))
        .await
    }

    /// Wait for one decrypted audio datagram, or time out.
    pub async fn hear(&mut self) -> Result<Option<udp::Audio>> {
        let socket = self.udp.clone().context("no UDP socket")?;
        let mut buffer = vec![0u8; 2048];

        let deadline = std::time::Duration::from_millis(1500);
        let received = tokio::time::timeout(deadline, async {
            loop {
                let read = socket.recv(&mut buffer).await?;
                let sealed = buffer.get(..read).unwrap_or_default().to_vec();
                let Some(crypt) = self.crypt.as_mut() else {
                    continue;
                };
                let Some(plaintext) = crypt.decrypt(&sealed) else {
                    continue;
                };
                if let Ok(UdpMessage::Audio(audio)) = decode_udp(&plaintext) {
                    return Ok::<udp::Audio, anyhow::Error>(audio);
                }
            }
        })
        .await;

        received.ok().transpose()
    }
}
