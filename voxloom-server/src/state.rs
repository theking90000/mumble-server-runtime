//! Minimal in-memory server state for Phase 3.
//!
//! This is server-local session bookkeeping — a session-id allocator, the static
//! channel tree, and the set of currently connected users — NOT the canonical
//! business state of P7. Keeping it separate honours R5 (a crate/phase only fills
//! what it needs): P3 needs enough state to run a handshake, associate a UDP peer
//! and reflect loopback audio, and nothing more.
//!
//! Concurrency (ADR follows the P2 proxy pattern): the registry is a plain
//! `std::sync::Mutex`. The lock is only ever held for synchronous work (map
//! lookups, `Arc` clones, OCB2 on one datagram — a few microseconds) and never
//! across an `.await`. Socket writes happen after the lock is released.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::mpsc;
use voxloom_audio::{AudioRoutingSnapshot, Participant, RoutingDomainId, compile};
use voxloom_crypto::CryptState;
use voxloom_protocol::ControlMessage;

use crate::config::ServerConfig;
use crate::limits::VoiceBudget;

/// A Mumble user session id. Monotonic per process (spec §9.1).
pub type SessionId = u32;

/// The root channel always uses id 0 (spec §11.2 invariant).
pub const ROOT_CHANNEL_ID: u32 = 0;

/// A static channel in the server's tree. P3 renders one fixed tree for every
/// connection (per-connection projection is P6); this is just enough structure
/// for the handshake and the §20 ordering invariants to be exercised.
#[derive(Debug, Clone)]
pub struct ChannelDef {
    pub id: u32,
    /// Parent channel id. The root's parent is itself (id 0) and is not emitted.
    pub parent: u32,
    pub name: String,
    pub position: i32,
}

/// A message queued for a connection's TCP writer. Control frames and
/// TCP-tunnelled audio (`UDPTunnel`) both travel as [`ControlMessage`].
pub type Outbound = ControlMessage;

/// One connected, authenticated user.
///
/// Shared behind an `Arc` so the owning connection task, the presence broadcast
/// from other connections, and the UDP voice plane can all reach it. Interior
/// fields that change after auth are individually locked; `session` and `name`
/// are immutable once set.
pub struct UserEntry {
    pub session: SessionId,
    pub name: String,
    /// Channel the user is shown in. Fixed to the root in P3 (moves are refused,
    /// fail-closed) but kept as state for later phases.
    pub channel_id: u32,
    /// Queue to this user's TCP writer. Other connections push presence updates
    /// (`UserState`/`UserRemove`) here; the voice plane pushes tunnelled audio.
    pub outbound: mpsc::UnboundedSender<Outbound>,
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

/// The mutable part of the server state, guarded by one mutex.
struct Registry {
    users: BTreeMap<SessionId, Arc<UserEntry>>,
    /// Reverse index address -> session for fast UDP correlation of a peer that
    /// has already proven itself.
    udp_bindings: HashMap<SocketAddr, SessionId>,
    /// The published routing table. Recompiled here, inside the same critical
    /// section that changes membership, so a reader can never observe a snapshot
    /// that disagrees with the user list it was built from.
    routes: Arc<AudioRoutingSnapshot>,
}

impl Registry {
    /// Recompile the routing table from the current membership (ADR-005's cold
    /// path). Called on every membership change and nowhere else: the packet
    /// path reads the result, it never triggers this.
    fn republish_routes(&mut self, generation: u64) {
        let participants: Vec<Participant> = self
            .users
            .keys()
            .map(|session| {
                Participant::new(
                    voxloom_audio::SessionId::new(*session),
                    // One domain until partitioning becomes real (Phase 7).
                    RoutingDomainId::DEFAULT,
                )
            })
            .collect();
        self.routes = Arc::new(compile(&participants, generation));
    }
}

/// Shared server state handed to every connection task and the voice plane.
pub struct SharedState {
    registry: Mutex<Registry>,
    /// Monotonic session-id source. Starts at 1; 0 is reserved (no user is
    /// session 0, matching Murmur which dequeues ids starting at 1).
    next_session: AtomicU32,
    config: ServerConfig,
    channels: Vec<ChannelDef>,
    /// Monotonic generation stamped onto each published routing table.
    route_generation: AtomicU64,
}

impl SharedState {
    /// Build shared state with the given config and a single root channel.
    pub fn new(config: ServerConfig) -> Arc<Self> {
        let root = ChannelDef {
            id: ROOT_CHANNEL_ID,
            parent: ROOT_CHANNEL_ID,
            name: config.server_name.clone(),
            position: 0,
        };
        Self::with_channels(config, vec![root])
    }

    /// Build shared state with an explicit channel tree (used by tests that want
    /// to exercise the parents-before-children ordering).
    pub fn with_channels(config: ServerConfig, channels: Vec<ChannelDef>) -> Arc<Self> {
        Arc::new(Self {
            registry: Mutex::new(Registry {
                users: BTreeMap::new(),
                udp_bindings: HashMap::new(),
                routes: Arc::new(compile(&[], 0)),
            }),
            next_session: AtomicU32::new(1),
            config,
            channels,
            route_generation: AtomicU64::new(0),
        })
    }

    /// The current routing table (spec 23.2).
    ///
    /// This is the publication mechanism: the reader clones an `Arc` out of the
    /// registry and works from an immutable generation that cannot change under
    /// it. The lock is held only for that clone, never across an await and never
    /// while a packet is being routed.
    pub fn routing_snapshot(&self) -> Arc<AudioRoutingSnapshot> {
        let registry = match self.registry.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        Arc::clone(&registry.routes)
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub fn channels(&self) -> &[ChannelDef] {
        &self.channels
    }

    /// Allocate the next monotonic session id (spec §9.1).
    pub fn allocate_session(&self) -> SessionId {
        self.next_session.fetch_add(1, Ordering::Relaxed)
    }

    /// Register a freshly authenticated user and return the snapshot of users
    /// that were already present (so the new connection can render them in its
    /// handshake). The new user is inserted atomically with reading the others.
    pub fn insert_user(&self, entry: Arc<UserEntry>) -> Vec<Arc<UserEntry>> {
        let mut registry = match self.registry.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let others: Vec<Arc<UserEntry>> = registry.users.values().cloned().collect();
        registry.users.insert(entry.session, entry);
        let generation = self.route_generation.fetch_add(1, Ordering::Relaxed) + 1;
        registry.republish_routes(generation);
        others
    }

    /// Remove a user (on disconnect) and drop any UDP binding it held.
    pub fn remove_user(&self, session: SessionId) {
        let mut registry = match self.registry.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        registry.users.remove(&session);
        registry.udp_bindings.retain(|_, bound| *bound != session);
        let generation = self.route_generation.fetch_add(1, Ordering::Relaxed) + 1;
        registry.republish_routes(generation);
    }

    /// Resolve a routing table's recipient list to live connections, taking the
    /// registry lock once for the whole packet rather than once per recipient.
    /// Sessions that left between compilation and delivery are simply absent.
    pub fn users_for(&self, sessions: &[voxloom_audio::SessionId]) -> Vec<Arc<UserEntry>> {
        let registry = match self.registry.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        sessions
            .iter()
            .filter_map(|session| registry.users.get(&session.get()).cloned())
            .collect()
    }

    /// Look up one connected user by session.
    pub fn user(&self, session: SessionId) -> Option<Arc<UserEntry>> {
        let registry = match self.registry.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        registry.users.get(&session).cloned()
    }

    /// All currently connected users.
    pub fn users(&self) -> Vec<Arc<UserEntry>> {
        let registry = match self.registry.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        registry.users.values().cloned().collect()
    }

    /// Look up the user a UDP address is already bound to, if any.
    pub fn user_for_addr(&self, addr: &SocketAddr) -> Option<Arc<UserEntry>> {
        let registry = match self.registry.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let session = registry.udp_bindings.get(addr)?;
        registry.users.get(session).cloned()
    }

    /// Bind a UDP address to a session after successful cryptographic proof.
    pub fn bind_udp(&self, addr: SocketAddr, session: SessionId) {
        let mut registry = match self.registry.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(entry) = registry.users.get(&session)
            && let Ok(mut slot) = entry.udp_addr.lock()
        {
            *slot = Some(addr);
        }
        registry.udp_bindings.insert(addr, session);
    }
}
