use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use mumble_server_runtime_crypto::{BLOCK_SIZE, CryptState, KEY_SIZE};
use mumble_server_runtime_protocol::messages::{tcp, udp};
use mumble_server_runtime_protocol::{
    ControlMessage, UdpMessage, decode_frame, decode_udp, encode_frame, encode_udp, parse_frame,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error as TlsError, SignatureScheme};
use serde::Serialize;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::task::{JoinError, JoinHandle};
use tokio::time::{Instant, Interval, MissedTickBehavior};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::audio::{OPUS_FRAME_DURATION, VoiceClip};
use crate::config::Config;
use crate::scenario::Scenario;
use crate::stats::ClientReport;

const EVENT_CAPACITY: usize = 256;
const ACTION_CAPACITY: usize = 32;

#[derive(Clone, PartialEq, Eq)]
pub struct MumbleCredential {
    pub username: String,
    password: String,
}

impl MumbleCredential {
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
        }
    }

    fn password(&self) -> &str {
        &self.password
    }
}

impl fmt::Debug for MumbleCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MumbleCredential")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub enum ClientAction {
    ReplaceCredential(MumbleCredential),
    SetVoiceEnabled(bool),
    Disconnect,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientEvent {
    Synchronized {
        monotonic_micros: u64,
        channel: String,
        mute: bool,
        deaf: bool,
        self_mute: bool,
        self_deaf: bool,
    },
    StateChanged {
        monotonic_micros: u64,
        channel: String,
        mute: bool,
        deaf: bool,
        self_mute: bool,
        self_deaf: bool,
    },
    CredentialReplaced {
        monotonic_micros: u64,
    },
    Disconnected {
        monotonic_micros: u64,
    },
    Failed {
        monotonic_micros: u64,
        message: String,
    },
}

#[derive(Debug, Error)]
pub enum ClientControlError {
    #[error("managed Mumble client is no longer accepting actions")]
    ActionChannelClosed,
    #[error("managed Mumble client task failed")]
    TaskFailed(#[source] JoinError),
    #[error("spawn_managed requires a Tokio runtime")]
    MissingRuntime(#[source] tokio::runtime::TryCurrentError),
}

#[derive(Debug, Clone)]
pub struct ManagedClientConfig {
    pub config: Config,
    pub credential: MumbleCredential,
    pub client_number: usize,
    pub voice_clip: Option<Arc<VoiceClip>>,
    pub voice_enabled: bool,
}

pub struct ManagedClient {
    actions: mpsc::Sender<ClientAction>,
    events: mpsc::Receiver<ClientEvent>,
    task: JoinHandle<ClientReport>,
}

impl ManagedClient {
    pub async fn send(&self, action: ClientAction) -> Result<(), ClientControlError> {
        self.actions
            .send(action)
            .await
            .map_err(|_| ClientControlError::ActionChannelClosed)
    }

    pub async fn next_event(&mut self) -> Option<ClientEvent> {
        self.events.recv().await
    }

    pub async fn wait(self) -> Result<ClientReport, ClientControlError> {
        let ManagedClient {
            actions,
            events,
            task,
        } = self;
        drop(actions);
        let report = task.await.map_err(ClientControlError::TaskFailed)?;
        drop(events);
        Ok(report)
    }
}

pub fn spawn_managed(config: ManagedClientConfig) -> Result<ManagedClient, ClientControlError> {
    let runtime =
        tokio::runtime::Handle::try_current().map_err(ClientControlError::MissingRuntime)?;
    let (action_sender, action_receiver) = mpsc::channel(ACTION_CAPACITY);
    let (event_sender, event_receiver) = mpsc::channel(EVENT_CAPACITY);
    let task = runtime.spawn(run_managed(config, action_receiver, event_sender));
    Ok(ManagedClient {
        actions: action_sender,
        events: event_receiver,
        task,
    })
}

#[derive(Debug, Clone, Copy)]
struct TalkSchedule {
    active: Duration,
    cycle: Option<Duration>,
    phase: Duration,
}

impl TalkSchedule {
    fn new(percent: u8, spurt: Duration, client_number: usize, clients: usize) -> Result<Self> {
        let Some(cycle) = spurt
            .checked_mul(100)
            .map(|total| total / u32::from(percent.max(1)))
        else {
            bail!("--talk-spurt is too large to derive a talk cycle");
        };
        let phase = if percent == 0 {
            Duration::ZERO
        } else {
            cycle.mul_f64(client_number as f64 / clients as f64)
        };
        Ok(Self {
            active: spurt,
            cycle: (percent > 0).then_some(cycle),
            phase,
        })
    }

    // REF: runtime/references/mumble/src/mumble/AudioInput.cpp:AudioInput::encodeAudioFrame
    // and AudioInput::flushCheck. Sustained silence sends nothing; the last
    // packet of a talking burst is marked as a terminator.
    fn packet_at(self, elapsed: Duration, packet_interval: Duration) -> Option<bool> {
        let cycle = self.cycle?;
        let position = (elapsed.as_nanos() + self.phase.as_nanos()) % cycle.as_nanos();
        if position >= self.active.as_nanos() {
            return None;
        }
        let ends_spurt =
            position.saturating_add(packet_interval.as_nanos()) >= self.active.as_nanos();
        Some(ends_spurt)
    }
}

#[derive(Debug, Default)]
pub struct Model {
    pub channels: BTreeMap<u32, Channel>,
    pub users: BTreeMap<u32, User>,
    pub session: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    pub name: String,
    pub parent: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub name: String,
    pub channel: u32,
    pub mute: bool,
    pub deaf: bool,
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

    pub fn self_channel_name(&self) -> Option<&str> {
        let session = self.session?;
        let user = self.users.get(&session)?;
        self.channels
            .get(&user.channel)
            .map(|channel| channel.name.as_str())
    }

    fn self_observation(&self) -> Option<SelfObservation> {
        let session = self.session?;
        let user = self.users.get(&session)?;
        let channel = self.channels.get(&user.channel)?;
        Some(SelfObservation {
            channel: channel.name.clone(),
            mute: user.mute,
            deaf: user.deaf,
            self_mute: user.self_mute,
            self_deaf: user.self_deaf,
        })
    }

    // REF: runtime/references/vendored/Mumble.proto:ChannelState, UserState, and ServerSync.
    fn apply(&mut self, message: &ControlMessage) {
        match message {
            ControlMessage::ChannelState(state) => {
                let Some(id) = state.channel_id else {
                    return;
                };
                let channel = self.channels.entry(id).or_insert(Channel {
                    name: String::new(),
                    parent: state.parent,
                });
                if let Some(name) = &state.name {
                    channel.name.clone_from(name);
                }
                if state.parent.is_some() {
                    channel.parent = state.parent;
                }
            }
            ControlMessage::ChannelRemove(remove) => {
                self.channels.remove(&remove.channel_id);
            }
            ControlMessage::UserState(state) => {
                let Some(session) = state.session else {
                    return;
                };
                let user = self.users.entry(session).or_insert(User {
                    name: String::new(),
                    channel: 0,
                    mute: false,
                    deaf: false,
                    self_mute: false,
                    self_deaf: false,
                });
                if let Some(name) = &state.name {
                    user.name.clone_from(name);
                }
                if let Some(channel) = state.channel_id {
                    user.channel = channel;
                }
                if let Some(mute) = state.mute {
                    user.mute = mute;
                }
                if let Some(deaf) = state.deaf {
                    user.deaf = deaf;
                }
                if let Some(self_mute) = state.self_mute {
                    user.self_mute = self_mute;
                }
                if let Some(self_deaf) = state.self_deaf {
                    user.self_deaf = self_deaf;
                }
            }
            ControlMessage::UserRemove(remove) => {
                self.users.remove(&remove.session);
            }
            ControlMessage::ServerSync(sync) => self.session = sync.session,
            _ => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelfObservation {
    channel: String,
    mute: bool,
    deaf: bool,
    self_mute: bool,
    self_deaf: bool,
}

/// This executable targets locally controlled servers whose generated
/// certificates intentionally change between runs.
#[derive(Debug)]
pub struct AcceptAnyServer;

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

pub fn tls_connector() -> TlsConnector {
    // TLS 1.2 mirrors the compatibility constraint recorded in the shard guide §15.
    let config = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS12])
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyServer))
        .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
}

pub async fn run(
    config: Arc<Config>,
    voice_clip: Option<Arc<VoiceClip>>,
    connector: TlsConnector,
    client_number: usize,
    launch_at: Instant,
    stop_at: Instant,
) -> ClientReport {
    tokio::time::sleep_until(launch_at).await;
    let mut report = ClientReport::default();
    let result = run_inner(
        &config,
        voice_clip,
        connector,
        client_number,
        stop_at,
        &mut report,
    )
    .await;
    match result {
        Ok(()) => report.completed = true,
        Err(error) => {
            report.error = Some(format!("client {client_number}: {error:#}"));
        }
    }
    report
}

async fn run_inner(
    config: &Config,
    voice_clip: Option<Arc<VoiceClip>>,
    connector: TlsConnector,
    client_number: usize,
    stop_at: Instant,
    report: &mut ClientReport,
) -> Result<()> {
    let username = format!("{}-{client_number}", config.username_prefix);
    let mut active = connect_client(
        config,
        connector,
        &username,
        config.password.as_deref(),
        report,
    )
    .await?;
    active.open_udp(report).await?;
    let exit = active
        .run(config, voice_clip, client_number, stop_at, report, None)
        .await?;
    ensure!(
        matches!(exit, ConnectionExit::Deadline),
        "unmanaged client received a managed action"
    );
    Ok(())
}

async fn connect_client(
    config: &Config,
    connector: TlsConnector,
    username: &str,
    password: Option<&str>,
    report: &mut ClientReport,
) -> Result<ActiveClient> {
    let tcp_started = Instant::now();
    let tcp = tokio::time::timeout(config.connect_timeout, TcpStream::connect(config.server))
        .await
        .context("TCP connect timeout")?
        .context("TCP connect")?;
    report.tcp_connect = Some(tcp_started.elapsed());

    let tls_started = Instant::now();
    let server_name = ServerName::try_from("localhost")?;
    let tls = tokio::time::timeout(
        config.handshake_timeout,
        connector.connect(server_name, tcp),
    )
    .await
    .context("TLS handshake timeout")?
    .context("TLS handshake")?;
    report.tls_handshake = Some(tls_started.elapsed());

    let (reader, mut writer) = tokio::io::split(tls);
    send_initial(&mut writer, username, password).await?;

    let protocol_started = Instant::now();
    let active = tokio::time::timeout(
        config.handshake_timeout,
        finish_handshake(reader, writer, config.server, report),
    )
    .await
    .context("Mumble synchronization timeout")??;
    report.protocol_handshake = Some(protocol_started.elapsed());

    ensure!(
        active
            .model
            .session
            .is_some_and(|session| active.model.users.contains_key(&session)),
        "ServerSync arrived before the local user existed"
    );
    ensure!(
        active.model.channels.contains_key(&0),
        "ServerSync arrived before root channel 0 existed"
    );
    ensure!(
        active.crypt.is_some(),
        "ServerSync arrived without a complete CryptSetup"
    );

    Ok(active)
}

async fn run_managed(
    managed: ManagedClientConfig,
    mut actions: mpsc::Receiver<ClientAction>,
    events: mpsc::Sender<ClientEvent>,
) -> ClientReport {
    let _installed = rustls::crypto::ring::default_provider().install_default();
    let connector = tls_connector();
    let started = Instant::now();
    let stop_at = started + managed.config.duration;
    let mut credential = managed.credential;
    let mut voice_enabled = managed.voice_enabled;
    let mut report = ClientReport::default();

    loop {
        let connected = connect_client(
            &managed.config,
            connector.clone(),
            &credential.username,
            Some(credential.password()),
            &mut report,
        )
        .await;
        let mut active = match connected {
            Ok(active) => active,
            Err(error) => {
                let message = format!("client {}: {error:#}", managed.client_number);
                if emit_event(
                    &events,
                    ClientEvent::Failed {
                        monotonic_micros: elapsed_micros(started),
                        message: message.clone(),
                    },
                )
                .is_err()
                {
                    report.error = Some(message);
                    return report;
                }
                report.error = Some(message);
                return report;
            }
        };
        if let Err(error) = active.open_udp(&mut report).await {
            report.error = Some(format!("client {}: {error:#}", managed.client_number));
            return report;
        }
        let Some(observation) = active.model.self_observation() else {
            report.error = Some(format!(
                "client {}: synchronized without a self observation",
                managed.client_number
            ));
            return report;
        };
        if emit_event(&events, event_from_observation(started, observation, true)).is_err() {
            report.error = Some("managed client event receiver closed".to_owned());
            return report;
        }

        let control = ManagedConnection {
            actions: &mut actions,
            events: &events,
            started,
            voice_enabled,
        };
        let exit = active
            .run(
                &managed.config,
                managed.voice_clip.as_ref().map(Arc::clone),
                managed.client_number,
                stop_at,
                &mut report,
                Some(control),
            )
            .await;
        match exit {
            Ok(ConnectionExit::Deadline | ConnectionExit::Stop) => {
                report.completed = true;
                return report;
            }
            Ok(ConnectionExit::ReplaceCredential(replacement, enabled)) => {
                credential = replacement;
                voice_enabled = enabled;
                report.reconnects = report.reconnects.saturating_add(1);
                if emit_event(
                    &events,
                    ClientEvent::CredentialReplaced {
                        monotonic_micros: elapsed_micros(started),
                    },
                )
                .is_err()
                {
                    report.error = Some("managed client event receiver closed".to_owned());
                    return report;
                }
            }
            Ok(ConnectionExit::Disconnected(enabled)) => {
                voice_enabled = enabled;
                if emit_event(
                    &events,
                    ClientEvent::Disconnected {
                        monotonic_micros: elapsed_micros(started),
                    },
                )
                .is_err()
                {
                    report.error = Some("managed client event receiver closed".to_owned());
                    return report;
                }
                match wait_for_reconnect(&mut actions, &mut voice_enabled).await {
                    Some(replacement) => {
                        credential = replacement;
                        report.reconnects = report.reconnects.saturating_add(1);
                    }
                    None => {
                        report.completed = true;
                        return report;
                    }
                }
            }
            Err(error) => {
                let message = format!("client {}: {error:#}", managed.client_number);
                if emit_event(
                    &events,
                    ClientEvent::Failed {
                        monotonic_micros: elapsed_micros(started),
                        message: message.clone(),
                    },
                )
                .is_err()
                {
                    report.error = Some(message);
                    return report;
                }
                report.error = Some(message);
                return report;
            }
        }
    }
}

async fn wait_for_reconnect(
    actions: &mut mpsc::Receiver<ClientAction>,
    voice_enabled: &mut bool,
) -> Option<MumbleCredential> {
    while let Some(action) = actions.recv().await {
        match action {
            ClientAction::ReplaceCredential(credential) => return Some(credential),
            ClientAction::SetVoiceEnabled(enabled) => *voice_enabled = enabled,
            ClientAction::Disconnect => {}
            ClientAction::Stop => return None,
        }
    }
    None
}

fn emit_event(events: &mpsc::Sender<ClientEvent>, event: ClientEvent) -> Result<()> {
    events
        .try_send(event)
        .map_err(|error| anyhow!("managed client event backpressure: {error}"))
}

fn elapsed_micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn event_from_observation(
    started: Instant,
    observation: SelfObservation,
    synchronized: bool,
) -> ClientEvent {
    if synchronized {
        ClientEvent::Synchronized {
            monotonic_micros: elapsed_micros(started),
            channel: observation.channel,
            mute: observation.mute,
            deaf: observation.deaf,
            self_mute: observation.self_mute,
            self_deaf: observation.self_deaf,
        }
    } else {
        ClientEvent::StateChanged {
            monotonic_micros: elapsed_micros(started),
            channel: observation.channel,
            mute: observation.mute,
            deaf: observation.deaf,
            self_mute: observation.self_mute,
            self_deaf: observation.self_deaf,
        }
    }
}

// REF: runtime/references/mumble/src/Mumble.proto:Version and Authenticate.
// REF: runtime/references/mumble/src/mumble/ServerHandler.cpp:ServerHandler::sendAuthenticate.
async fn send_initial(
    writer: &mut WriteHalf<TlsStream<TcpStream>>,
    username: &str,
    password: Option<&str>,
) -> Result<()> {
    let version = ControlMessage::Version(tcp::Version {
        // REF: runtime/references/mumble/src/Version.h:Version::fromComponents.
        version_v2: Some((1u64 << 48) | (5u64 << 32)),
        release: Some("mumble-server-runtime-stress".to_owned()),
        os: Some(std::env::consts::OS.to_owned()),
        ..Default::default()
    });
    send_control(writer, &version).await?;
    send_control(
        writer,
        &ControlMessage::Authenticate(tcp::Authenticate {
            username: Some(username.to_owned()),
            password: password.map(str::to_owned),
            opus: Some(true),
            client_type: Some(1),
            ..Default::default()
        }),
    )
    .await
}

struct ActiveClient {
    reader: ReadHalf<TlsStream<TcpStream>>,
    writer: WriteHalf<TlsStream<TcpStream>>,
    buffer: Vec<u8>,
    model: Model,
    crypt: Option<CryptState>,
    server: SocketAddr,
    udp: Option<UdpSocket>,
}

struct ManagedConnection<'a> {
    actions: &'a mut mpsc::Receiver<ClientAction>,
    events: &'a mpsc::Sender<ClientEvent>,
    started: Instant,
    voice_enabled: bool,
}

enum ConnectionExit {
    Deadline,
    Stop,
    Disconnected(bool),
    ReplaceCredential(MumbleCredential, bool),
}

async fn finish_handshake(
    reader: ReadHalf<TlsStream<TcpStream>>,
    writer: WriteHalf<TlsStream<TcpStream>>,
    server: SocketAddr,
    report: &mut ClientReport,
) -> Result<ActiveClient> {
    let mut active = ActiveClient {
        reader,
        writer,
        buffer: Vec::with_capacity(4096),
        model: Model::default(),
        crypt: None,
        server,
        udp: None,
    };

    loop {
        let message = read_control(&mut active.reader, &mut active.buffer).await?;
        report.tcp_frames_received = report.tcp_frames_received.saturating_add(1);
        active.model.apply(&message);
        match message {
            ControlMessage::CryptSetup(setup) => {
                handle_crypt_setup(&mut active.crypt, &mut active.writer, &setup).await?;
            }
            ControlMessage::Reject(reject) => {
                bail!(
                    "server rejected authentication: {}",
                    reject.reason.as_deref().unwrap_or("no reason")
                );
            }
            ControlMessage::ServerSync(_) => return Ok(active),
            _ => {}
        }
    }
}

impl ActiveClient {
    async fn open_udp(&mut self, report: &mut ClientReport) -> Result<()> {
        let bind = if self.server.is_ipv4() {
            SocketAddr::from(([0, 0, 0, 0], 0))
        } else {
            "[::]:0".parse().context("IPv6 wildcard address")?
        };
        let udp = UdpSocket::bind(bind).await.context("binding UDP")?;
        udp.connect(self.server).await.context("connecting UDP")?;
        self.udp = Some(udp);
        self.send_udp(
            &UdpMessage::Ping(udp::Ping {
                // Proto3 omits a zero value, which would leave an invalid
                // header-only envelope. Any nonzero opaque value is valid.
                timestamp: 1,
                ..Default::default()
            }),
            report,
        )
        .await
    }

    async fn run(
        &mut self,
        config: &Config,
        voice_clip: Option<Arc<VoiceClip>>,
        client_number: usize,
        stop_at: Instant,
        report: &mut ClientReport,
        mut managed: Option<ManagedConnection<'_>>,
    ) -> Result<ConnectionExit> {
        let mut scenario = Scenario::new(config.scenario, client_number);
        let mut tcp_ping = interval(config.ping_interval);
        let mut udp_ping = interval(config.ping_interval);
        let mut voice = voice_clip.as_ref().map(|_| interval(OPUS_FRAME_DURATION));
        let mut interaction = interval(config.interaction_interval);
        let mut deadline = Box::pin(tokio::time::sleep_until(stop_at));
        let mut udp_buffer = vec![0u8; 2048];
        let mut ping_sequence = 2u64;
        let mut pending_tcp_ping: Option<(u64, Instant)> = None;
        let mut pending_udp_ping: Option<(u64, Instant)> = None;
        let mut frame_number = 0u64;
        let mut voice_frame_index = 0usize;
        let talk_schedule = TalkSchedule::new(
            config.talk_percent,
            config.talk_spurt,
            client_number,
            config.clients.get(),
        )?;
        let voice_started = Instant::now();

        loop {
            let udp = self.udp.as_ref().context("UDP was not opened")?;
            enum Event {
                Stop,
                Control(Box<Result<ControlMessage>>),
                Datagram(std::io::Result<usize>),
                TcpPing,
                UdpPing,
                Voice,
                Interaction,
                Action(ClientAction),
            }
            let event = tokio::select! {
                // Cancellation-safe: Sleep retains its deadline when polled again.
                _ = &mut deadline => Event::Stop,
                // Cancellation-safe: Tokio AsyncReadExt::read may be cancelled without consuming bytes.
                message = read_control(&mut self.reader, &mut self.buffer) => Event::Control(Box::new(message)),
                // Cancellation-safe: UdpSocket::recv documents no partial datagram consumption.
                received = udp.recv(&mut udp_buffer) => Event::Datagram(received),
                // Cancellation-safe: an Interval tick can be abandoned; missed ticks are skipped.
                _ = tcp_ping.tick() => Event::TcpPing,
                // Cancellation-safe: an Interval tick can be abandoned; missed ticks are skipped.
                _ = udp_ping.tick() => Event::UdpPing,
                // Cancellation-safe: the optional interval owns no work until its tick completes.
                _ = optional_tick(&mut voice) => Event::Voice,
                // Cancellation-safe: scenario state changes only after this branch wins.
                _ = interaction.tick() => Event::Interaction,
                // Cancellation-safe: mpsc::Receiver::recv does not consume a message when cancelled.
                action = optional_action(&mut managed) => Event::Action(action),
            };

            match event {
                Event::Stop => {
                    self.writer
                        .shutdown()
                        .await
                        .context("closing the TLS control connection")?;
                    return Ok(ConnectionExit::Deadline);
                }
                Event::Control(message) => {
                    let message = (*message)?;
                    report.tcp_frames_received = report.tcp_frames_received.saturating_add(1);
                    let previous = self.model.self_observation();
                    self.model.apply(&message);
                    let current = self.model.self_observation();
                    if previous != current
                        && let (Some(observation), Some(control)) = (current, managed.as_ref())
                    {
                        emit_event(
                            control.events,
                            event_from_observation(control.started, observation, false),
                        )?;
                    }
                    match message {
                        ControlMessage::CryptSetup(setup) => {
                            handle_crypt_setup(&mut self.crypt, &mut self.writer, &setup).await?;
                        }
                        ControlMessage::Ping(ping) => {
                            if pending_tcp_ping
                                .is_some_and(|(sent, _)| ping.timestamp == Some(sent))
                                && let Some((_, started)) = pending_tcp_ping.take()
                            {
                                report.tcp_ping_rtts.push(started.elapsed());
                            }
                        }
                        ControlMessage::UdpTunnel(raw) => {
                            if matches!(decode_udp(&raw), Ok(UdpMessage::Audio(_))) {
                                report.voice_packets_received =
                                    report.voice_packets_received.saturating_add(1);
                            }
                        }
                        ControlMessage::PermissionDenied(_) => {
                            report.denied_interactions =
                                report.denied_interactions.saturating_add(1);
                        }
                        ControlMessage::Reject(reject) => {
                            bail!(
                                "server rejected an established client: {}",
                                reject.reason.as_deref().unwrap_or("no reason")
                            );
                        }
                        _ => {}
                    }
                }
                Event::Datagram(received) => {
                    let received = received.context("receiving UDP")?;
                    report.udp_packets_received = report.udp_packets_received.saturating_add(1);
                    let sealed = udp_buffer.get(..received).unwrap_or_default();
                    let crypt = self.crypt.as_mut().context("missing UDP crypto")?;
                    let plaintext = crypt
                        .decrypt(sealed)
                        .ok_or_else(|| anyhow!("server sent an invalid encrypted UDP packet"))?;
                    match decode_udp(&plaintext).context("decoding server UDP")? {
                        UdpMessage::Ping(ping) => {
                            if pending_udp_ping.is_some_and(|(sent, _)| ping.timestamp == sent)
                                && let Some((_, started)) = pending_udp_ping.take()
                            {
                                report.udp_ping_rtts.push(started.elapsed());
                            }
                        }
                        UdpMessage::Audio(_) => {
                            report.voice_packets_received =
                                report.voice_packets_received.saturating_add(1);
                        }
                    }
                }
                Event::TcpPing => {
                    let timestamp = ping_sequence;
                    ping_sequence = ping_sequence.saturating_add(1);
                    let (good, late, lost) = self
                        .crypt
                        .as_ref()
                        .map_or((0, 0, 0), |crypt| (crypt.good, crypt.late, crypt.lost));
                    send_control(
                        &mut self.writer,
                        &ControlMessage::Ping(tcp::Ping {
                            timestamp: Some(timestamp),
                            good: Some(good),
                            late: Some(late),
                            lost: Some(lost),
                            resync: Some(0),
                            udp_packets: u32::try_from(report.udp_packets_received).ok(),
                            tcp_packets: u32::try_from(report.tcp_frames_received).ok(),
                            ..Default::default()
                        }),
                    )
                    .await?;
                    report.tcp_pings_sent = report.tcp_pings_sent.saturating_add(1);
                    pending_tcp_ping = Some((timestamp, Instant::now()));
                }
                Event::UdpPing => {
                    let timestamp = ping_sequence;
                    ping_sequence = ping_sequence.saturating_add(1);
                    self.send_udp(
                        &UdpMessage::Ping(udp::Ping {
                            timestamp,
                            ..Default::default()
                        }),
                        report,
                    )
                    .await?;
                    pending_udp_ping = Some((timestamp, Instant::now()));
                }
                Event::Voice => {
                    let packet_frame = frame_number;
                    frame_number = frame_number.saturating_add(1);
                    let voice_enabled =
                        managed.as_ref().is_none_or(|control| control.voice_enabled);
                    if voice_enabled
                        && let Some(is_terminator) =
                            talk_schedule.packet_at(voice_started.elapsed(), OPUS_FRAME_DURATION)
                    {
                        let clip = voice_clip
                            .as_ref()
                            .context("voice event fired without an Opus clip")?;
                        let voice_payload = clip
                            .packet(voice_frame_index)
                            .context("prepared Opus clip has no packet")?
                            .to_vec();
                        voice_frame_index = clip.next_index(voice_frame_index);
                        self.send_udp(
                            &UdpMessage::Audio(udp::Audio {
                                header: Some(udp::audio::Header::Target(0)),
                                frame_number: packet_frame,
                                opus_data: voice_payload,
                                is_terminator,
                                ..Default::default()
                            }),
                            report,
                        )
                        .await?;
                        report.voice_packets_sent = report.voice_packets_sent.saturating_add(1);
                    }
                }
                Event::Interaction => {
                    if let Some(message) = scenario.next(&self.model) {
                        send_control(&mut self.writer, &message).await?;
                        report.interactions_sent = report.interactions_sent.saturating_add(1);
                    }
                }
                Event::Action(action) => match action {
                    ClientAction::SetVoiceEnabled(enabled) => {
                        if let Some(control) = managed.as_mut() {
                            control.voice_enabled = enabled;
                        }
                    }
                    ClientAction::Disconnect => {
                        self.writer
                            .shutdown()
                            .await
                            .context("closing the managed TLS control connection")?;
                        let enabled = managed
                            .as_ref()
                            .is_some_and(|control| control.voice_enabled);
                        return Ok(ConnectionExit::Disconnected(enabled));
                    }
                    ClientAction::ReplaceCredential(credential) => {
                        self.writer
                            .shutdown()
                            .await
                            .context("closing the replaced TLS control connection")?;
                        let enabled = managed
                            .as_ref()
                            .is_some_and(|control| control.voice_enabled);
                        return Ok(ConnectionExit::ReplaceCredential(credential, enabled));
                    }
                    ClientAction::Stop => {
                        self.writer
                            .shutdown()
                            .await
                            .context("stopping the managed TLS control connection")?;
                        return Ok(ConnectionExit::Stop);
                    }
                },
            }
        }
    }

    // REF: runtime/references/mumble/src/MumbleUDP.proto:Audio and Ping envelopes.
    async fn send_udp(&mut self, message: &UdpMessage, report: &mut ClientReport) -> Result<()> {
        let udp = self.udp.as_ref().context("UDP was not opened")?;
        let crypt = self.crypt.as_mut().context("missing UDP crypto")?;
        let sealed = crypt
            .encrypt(&encode_udp(message))
            .context("encrypting UDP")?;
        let sent = udp.send(&sealed).await.context("sending UDP")?;
        ensure!(sent == sealed.len(), "UDP send was truncated");
        report.udp_packets_sent = report.udp_packets_sent.saturating_add(1);
        Ok(())
    }
}

fn interval(period: Duration) -> Interval {
    let mut interval = tokio::time::interval_at(Instant::now() + period, period);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    interval
}

async fn optional_tick(interval: &mut Option<Interval>) {
    match interval {
        Some(interval) => {
            interval.tick().await;
        }
        None => std::future::pending().await,
    }
}

async fn optional_action(managed: &mut Option<ManagedConnection<'_>>) -> ClientAction {
    match managed {
        Some(control) => control.actions.recv().await.unwrap_or(ClientAction::Stop),
        None => std::future::pending().await,
    }
}

async fn read_control(
    reader: &mut (impl AsyncRead + Unpin),
    buffer: &mut Vec<u8>,
) -> Result<ControlMessage> {
    loop {
        if let Some(frame) = parse_frame(buffer)? {
            let consumed = frame.total_len();
            let message = decode_frame(&frame)?;
            buffer.drain(..consumed);
            return Ok(message);
        }
        let mut chunk = [0u8; 8192];
        let read = reader.read(&mut chunk).await.context("reading TCP")?;
        ensure!(read > 0, "server closed the TCP connection");
        buffer.extend_from_slice(chunk.get(..read).unwrap_or_default());
    }
}

async fn send_control(
    writer: &mut WriteHalf<TlsStream<TcpStream>>,
    message: &ControlMessage,
) -> Result<()> {
    let mut framed = Vec::new();
    encode_frame(message, &mut framed)?;
    writer.write_all(&framed).await.context("writing TCP")?;
    writer.flush().await.context("flushing TCP")
}

// REF: runtime/references/mumble/src/mumble/Messages.cpp:MainWindow::msgCryptSetup.
async fn handle_crypt_setup(
    crypt: &mut Option<CryptState>,
    writer: &mut WriteHalf<TlsStream<TcpStream>>,
    setup: &tcp::CryptSetup,
) -> Result<()> {
    match (&setup.key, &setup.client_nonce, &setup.server_nonce) {
        (Some(key), Some(client_nonce), Some(server_nonce)) => {
            let key = <[u8; KEY_SIZE]>::try_from(key.as_slice())
                .context("CryptSetup key is not 16 bytes")?;
            let client_nonce = <[u8; BLOCK_SIZE]>::try_from(client_nonce.as_slice())
                .context("CryptSetup client nonce is not 16 bytes")?;
            let server_nonce = <[u8; BLOCK_SIZE]>::try_from(server_nonce.as_slice())
                .context("CryptSetup server nonce is not 16 bytes")?;
            *crypt = Some(CryptState::new(&key, &client_nonce, &server_nonce));
            Ok(())
        }
        (None, None, Some(server_nonce)) => {
            let server_nonce = <[u8; BLOCK_SIZE]>::try_from(server_nonce.as_slice())
                .context("CryptSetup server nonce is not 16 bytes")?;
            crypt
                .as_mut()
                .context("received nonce resync before key setup")?
                .set_decrypt_iv(&server_nonce);
            Ok(())
        }
        (None, None, None) => {
            let client_nonce = crypt
                .as_ref()
                .context("received nonce request before key setup")?
                .encrypt_iv()
                .to_vec();
            send_control(
                writer,
                &ControlMessage::CryptSetup(tcp::CryptSetup {
                    client_nonce: Some(client_nonce),
                    ..Default::default()
                }),
            )
            .await
        }
        _ => bail!("unsupported partial CryptSetup from server"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_percent_spreads_one_talker_across_twenty_clients() -> Result<()> {
        let talking = (0..20)
            .map(|client| TalkSchedule::new(5, Duration::from_secs(2), client, 20))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|schedule| {
                schedule
                    .packet_at(Duration::ZERO, Duration::from_millis(20))
                    .is_some()
            })
            .count();

        assert_eq!(talking, 1);
        Ok(())
    }

    #[test]
    fn last_packet_in_a_spurt_is_a_terminator() -> Result<()> {
        let schedule = TalkSchedule::new(50, Duration::from_secs(2), 0, 1)?;

        assert_eq!(
            schedule.packet_at(Duration::ZERO, Duration::from_secs(1)),
            Some(false)
        );
        assert_eq!(
            schedule.packet_at(Duration::from_secs(1), Duration::from_secs(1)),
            Some(true)
        );
        assert_eq!(
            schedule.packet_at(Duration::from_secs(2), Duration::from_secs(1)),
            None
        );
        assert_eq!(
            schedule.packet_at(Duration::from_secs(4), Duration::from_secs(1)),
            Some(false)
        );
        Ok(())
    }

    #[test]
    fn zero_and_full_talk_percent_are_explicit_modes() -> Result<()> {
        let silent = TalkSchedule::new(0, Duration::from_secs(2), 0, 1)?;
        let continuous = TalkSchedule::new(100, Duration::from_secs(2), 0, 1)?;

        assert_eq!(
            silent.packet_at(Duration::ZERO, Duration::from_millis(20)),
            None
        );
        assert_eq!(
            continuous.packet_at(Duration::from_secs(9), Duration::from_millis(20)),
            Some(false)
        );
        Ok(())
    }

    #[test]
    fn credential_debug_redacts_bearer_password() {
        let credential = MumbleCredential::new("participant-7", "secret-token");
        let debug = format!("{credential:?}");

        assert!(debug.contains("participant-7"));
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("secret-token"));
    }

    #[test]
    fn self_observation_tracks_server_and_self_mutes() {
        let mut model = Model::default();
        model.apply(&ControlMessage::ChannelState(tcp::ChannelState {
            channel_id: Some(0),
            name: Some("Space alpha".to_owned()),
            ..Default::default()
        }));
        model.apply(&ControlMessage::UserState(tcp::UserState {
            session: Some(17),
            channel_id: Some(0),
            mute: Some(true),
            self_deaf: Some(true),
            ..Default::default()
        }));
        model.apply(&ControlMessage::ServerSync(tcp::ServerSync {
            session: Some(17),
            ..Default::default()
        }));

        assert_eq!(
            model.self_observation(),
            Some(SelfObservation {
                channel: "Space alpha".to_owned(),
                mute: true,
                deaf: false,
                self_mute: false,
                self_deaf: true,
            })
        );
    }
}
