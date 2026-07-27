//! In-memory server state: connections, their transports, and the coordinator.
//!
//! There is no business state here. Who exists, who sees whom and who may hear
//! whom is the compiled flavor's answer, published through
//! [`voxloom_control::PublicationCoordinator`]; this module owns only what a
//! socket needs: session ids, OCB2 state, UDP bindings and output queues.
//!
//! Concurrency (ADR follows the P2 proxy pattern): two plain `std::sync::Mutex`
//! guards, one over the connection registry and one over the coordinator. Both
//! are only ever held for synchronous work and never across an `.await`, and
//! never both at once: a publication copies the connection handles it needs,
//! releases the registry, and only then takes the coordinator.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use voxloom_audio::AudioRoutingSnapshot;
use voxloom_control::{PublicationCoordinator, PublishedGeneration};
use voxloom_crypto::CryptState;
use voxloom_flavor::ConnectionId;
use voxloom_protocol::ControlMessage;
use voxloom_render::SessionId as ViewSessionId;

use crate::config::ServerConfig;
use crate::flavor::FlavorRuntime;
use crate::limits::VoiceBudget;
use crate::outbound::{OutboundQueue, TransitionRefused};

/// A Mumble user session id. Monotonic per process (spec §9.1).
pub type SessionId = u32;

/// The root channel always uses id 0 (spec §11.2 invariant).
pub const ROOT_CHANNEL_ID: u32 = 0;

/// One connected, authenticated user.
///
/// Shared behind an `Arc` so the owning connection task, the publication path
/// and the UDP voice plane can all reach it. Interior fields that change after
/// auth are individually locked; `session`, `connection` and `name` are
/// immutable once set.
pub struct UserEntry {
    pub session: SessionId,
    /// The runtime identity the flavor sees. Distinct from the wire session on
    /// purpose: a flavor never learns a Mumble session id.
    pub connection: ConnectionId,
    pub name: String,
    /// SHA-1 digest of the immediate TLS client certificate, when supplied.
    /// This is presentation identity only and never an authorization input.
    pub certificate_hash: Option<String>,
    /// Queue to this user's TCP writer. Published generations push view frames
    /// here; the voice plane pushes tunnelled audio. It is bounded and refuses
    /// the two classes differently — see [`crate::outbound`].
    pub outbound: OutboundQueue,
    /// Per-connection OCB2 state. `None` until `CryptSetup` has been sent.
    pub crypto: Mutex<Option<CryptState>>,
    /// The UDP address bound to this session by cryptographic proof, if any. The
    /// IP alone never selects the session (spec §11.3).
    pub udp_addr: Mutex<Option<SocketAddr>>,
    /// Whether audio for this user should go out over UDP rather than the TCP
    /// tunnel. It follows the transport the user last sent audio on, which is
    /// exactly the real server's rule.
    ///
    /// REF: references/mumble/src/murmur/ServerUser.cpp : `aiUdpFlag = 1` at
    ///   construction.
    /// REF: references/mumble/src/murmur/Server.cpp : `u->aiUdpFlag = 1` when a
    ///   UDP audio packet is accepted, `u->aiUdpFlag = 0` in the `UDPTunnel`
    ///   branch, and `sendMessage` picks UDP only when the flag is set and the
    ///   user has a UDP socket.
    pub udp_mode: AtomicBool,
    /// Per-connection voice packet budget (spec 15.7).
    pub voice_budget: Mutex<VoiceBudget>,
}

impl UserEntry {
    /// Whether this user has completed UDP crypto setup and can be routed audio.
    pub fn has_crypto(&self) -> bool {
        self.crypto
            .lock()
            .map(|guard| guard.is_some())
            .unwrap_or(false)
    }

    /// The OCB2 counters `(good, late, lost)` for datagrams received from this
    /// user, or zeros if it has no crypto state yet. Reported back in the TCP
    /// `Ping` reply so the client can tell whether its UDP reaches us.
    pub fn crypt_counters(&self) -> (u32, u32, u32) {
        match self.crypto.lock() {
            Ok(guard) => match guard.as_ref() {
                Some(state) => (state.good, state.late, state.lost),
                None => (0, 0, 0),
            },
            Err(_) => (0, 0, 0),
        }
    }

    /// Where audio for this user should be sent: its bound UDP address, or
    /// `None` meaning "use the TCP tunnel".
    ///
    /// Both conditions of the real server are required: the user must have
    /// proven a UDP address *and* have last spoken over UDP. A user that fell
    /// back to the tunnel keeps receiving over the tunnel until its own
    /// datagrams reach us again.
    pub fn udp_destination(&self) -> Option<SocketAddr> {
        if !self.udp_mode.load(Ordering::Relaxed) {
            return None;
        }
        self.udp_addr.lock().ok().and_then(|guard| *guard)
    }

    /// Record which transport this user last sent audio on (`aiUdpFlag`).
    pub fn set_udp_mode(&self, over_udp: bool) {
        self.udp_mode.store(over_udp, Ordering::Relaxed);
    }

    /// Take one packet's worth of voice budget. `false` means drop (spec 15.7).
    pub fn allow_voice_packet(&self, now: Instant) -> bool {
        match self.voice_budget.lock() {
            Ok(mut budget) => budget.allow(now),
            // A poisoned budget must not become an unlimited one.
            Err(_) => false,
        }
    }
}

/// The mutable transport-side state, guarded by one mutex.
struct Registry {
    users: BTreeMap<SessionId, Arc<UserEntry>>,
    /// Reverse index address -> session for fast UDP correlation of a peer that
    /// has already proven itself.
    udp_bindings: HashMap<SocketAddr, SessionId>,
}

/// Shared server state handed to every connection task and the voice plane.
pub struct SharedState {
    registry: Mutex<Registry>,
    /// The control plane. Holding it serializes generations, which is exactly
    /// what spec 23.1 asks of the coordinator.
    coordinator: Mutex<PublicationCoordinator>,
    flavor: Arc<dyn FlavorRuntime>,
    /// Monotonic session-id source. Starts at 1; 0 is reserved (no user is
    /// session 0, matching Murmur which dequeues ids starting at 1).
    next_session: AtomicU32,
    /// Monotonic runtime-identity source, independent from the wire session.
    next_connection: AtomicU64,
    config: ServerConfig,
}

impl SharedState {
    /// Build shared state around one compiled flavor.
    pub fn new(config: ServerConfig, flavor: Arc<dyn FlavorRuntime>) -> Arc<Self> {
        Arc::new(Self {
            registry: Mutex::new(Registry {
                users: BTreeMap::new(),
                udp_bindings: HashMap::new(),
            }),
            coordinator: Mutex::new(PublicationCoordinator::new()),
            flavor,
            next_session: AtomicU32::new(1),
            next_connection: AtomicU64::new(1),
            config,
        })
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// The current routing table (spec 23.2).
    ///
    /// The reader clones an `Arc` out of the coordinator and works from an
    /// immutable generation that cannot change under it. The lock is held only
    /// for that clone, never across an await and never while routing a packet.
    pub fn routing_snapshot(&self) -> Arc<AudioRoutingSnapshot> {
        Arc::clone(self.coordinator().audio())
    }

    /// Allocate the next monotonic session id (spec §9.1).
    pub fn allocate_session(&self) -> SessionId {
        self.next_session.fetch_add(1, Ordering::Relaxed)
    }

    /// Build the server-owned live state for one authenticated connection and
    /// register it with the control plane.
    ///
    /// The flavor is told about the connection *before* it becomes renderable,
    /// so the first generation that includes it already knows who it is.
    pub fn admit(
        &self,
        session: SessionId,
        name: String,
        certificate_hash: Option<String>,
        outbound: OutboundQueue,
        crypt_state: CryptState,
        now: Instant,
    ) -> Arc<UserEntry> {
        let connection = ConnectionId::new(self.next_connection.fetch_add(1, Ordering::Relaxed));
        let entry = Arc::new(UserEntry {
            session,
            connection,
            name: name.clone(),
            certificate_hash: certificate_hash.clone(),
            outbound,
            crypto: Mutex::new(Some(crypt_state)),
            udp_addr: Mutex::new(None),
            udp_mode: AtomicBool::new(true),
            voice_budget: Mutex::new(VoiceBudget::new(now)),
        });

        {
            let mut registry = self.registry();
            registry.users.insert(session, Arc::clone(&entry));
        }

        let event = {
            let mut coordinator = self.coordinator();
            let event = coordinator.connected(connection, name, certificate_hash);
            if let Err(error) = coordinator.register(connection, ViewSessionId(session)) {
                eprintln!("voxloom-server: session {session}: cannot register: {error}");
            }
            event
        };
        self.flavor.report(&event);
        entry
    }

    /// Remove a connection (on disconnect) and drop any UDP binding it held.
    pub fn remove_user(&self, session: SessionId) {
        let entry = {
            let mut registry = self.registry();
            registry.udp_bindings.retain(|_, bound| *bound != session);
            registry.users.remove(&session)
        };
        let Some(entry) = entry else {
            return;
        };

        // The routing table loses the departed connection here, before the
        // remaining connections are told anything: a session that is gone must
        // stop being a possible recipient immediately.
        let event = {
            let mut coordinator = self.coordinator();
            coordinator.unregister(entry.connection);
            coordinator.disconnected(entry.connection, "connection closed")
        };
        self.flavor.report(&event);
    }

    /// Report one voice event to the flavor. Voxloom applies nothing itself; the
    /// flavor may or may not produce a new snapshot in response.
    pub fn report(&self, event: &voxloom_flavor::VoiceEvent) {
        self.flavor.report(event);
    }

    /// Resolve one inbound message against its sender's committed view.
    pub fn resolve_inbound(
        &self,
        connection: ConnectionId,
        message: &ControlMessage,
    ) -> Result<voxloom_control::InboundOutcome, voxloom_control::VoiceEventError> {
        self.coordinator().resolve_event(connection, message)
    }

    /// Publish one complete generation: render every connection from the
    /// flavor's current snapshot, deliver the view transitions, then grant the
    /// routes the committed views justify.
    ///
    /// Entirely synchronous: frames go to bounded queues that the connection
    /// tasks drain, so no `.await` happens under the coordinator lock. A
    /// connection whose queue refuses the transition keeps its previous view
    /// and is planned again by the next generation.
    pub fn publish_generation(&self) -> Option<PublishedGeneration> {
        let users: BTreeMap<ConnectionId, Arc<UserEntry>> = self
            .registry()
            .users
            .values()
            .map(|entry| (entry.connection, Arc::clone(entry)))
            .collect();

        let mut coordinator = self.coordinator();
        let pending = match self.flavor.plan(&mut coordinator) {
            Ok(pending) => pending,
            Err(error) => {
                eprintln!("voxloom-server: generation refused: {error}");
                return None;
            }
        };
        let (deliveries, mut commit) = pending.split();

        for (connection, messages) in deliveries {
            let Some(entry) = users.get(&connection) else {
                // The connection left between the copy above and here. Leaving
                // its token unspent keeps it out of the grant stage.
                continue;
            };
            match entry.outbound.send_transition(messages) {
                Ok(()) => {
                    if let Err(error) = coordinator.commit_connection(&mut commit, connection) {
                        eprintln!(
                            "voxloom-server: session {}: queued a transition it cannot commit: \
                             {error}",
                            entry.session
                        );
                        entry.outbound.mark_fatal();
                    }
                }
                Err(TransitionRefused::Congested { .. }) => {
                    // The writer retries after each drained message. This
                    // connection stays on its committed view; the others still
                    // advance.
                }
                Err(TransitionRefused::TooLarge { .. } | TransitionRefused::Closed) => {}
            }
        }

        match coordinator.finish(commit) {
            Ok(published) => Some(published),
            Err(error) => {
                eprintln!("voxloom-server: generation could not be closed: {error}");
                None
            }
        }
    }

    /// Resolve a routing table's recipient list to live connections, taking the
    /// registry lock once for the whole packet rather than once per recipient.
    /// Sessions that left between compilation and delivery are simply absent.
    pub fn users_for(&self, sessions: &[voxloom_audio::SessionId]) -> Vec<Arc<UserEntry>> {
        let registry = self.registry();
        sessions
            .iter()
            .filter_map(|session| registry.users.get(&session.get()).cloned())
            .collect()
    }

    /// Look up one connected user by session.
    pub fn user(&self, session: SessionId) -> Option<Arc<UserEntry>> {
        self.registry().users.get(&session).cloned()
    }

    /// All currently connected users.
    pub fn users(&self) -> Vec<Arc<UserEntry>> {
        self.registry().users.values().cloned().collect()
    }

    /// Look up the user a UDP address is already bound to, if any.
    pub fn user_for_addr(&self, addr: &SocketAddr) -> Option<Arc<UserEntry>> {
        let registry = self.registry();
        let session = registry.udp_bindings.get(addr)?;
        registry.users.get(session).cloned()
    }

    /// Bind a UDP address to a session after successful cryptographic proof.
    pub fn bind_udp(&self, addr: SocketAddr, session: SessionId) {
        let mut registry = self.registry();
        if let Some(entry) = registry.users.get(&session)
            && let Ok(mut slot) = entry.udp_addr.lock()
        {
            *slot = Some(addr);
        }
        registry.udp_bindings.insert(addr, session);
    }

    /// Take the registry lock, recovering from poisoning: the guarded maps have
    /// no invariant a panic could leave half-applied, and refusing every later
    /// connection would be a worse answer than continuing.
    fn registry(&self) -> std::sync::MutexGuard<'_, Registry> {
        match self.registry.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Take the coordinator lock, with the same poison recovery. A generation
    /// that panicked mid-publication leaves committed views untouched, because
    /// nothing is committed until its token is spent.
    fn coordinator(&self) -> std::sync::MutexGuard<'_, PublicationCoordinator> {
        match self.coordinator.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}
