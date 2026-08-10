//! What the voice plane needs to reach one connection, and how it finds it.
//!
//! This is the guide's `Bindings` table (9.7). A datagram arrives with nothing
//! but a source address, and everything needed to answer it has to be reachable
//! from that address alone: the OCB2 domain, the shard's routing table, the
//! cursor to gate on, and a queue for the TCP fallback.
//!
//! # Locks
//!
//! There are several, and none of them is on a hot path in the sense that
//! matters. Each [`Peer`] owns its own `Mutex`, so two connections never contend
//! with each other; the registry's `RwLock`s are read once per datagram and
//! written once per connection lifetime. No guard here is ever held across an
//! `.await`: every method returns before its caller can suspend.
//!
//! The one lock that would be a design error is a shared lock over *live state*
//! on the packet path. There is none: the routing table is an `Arc` that the
//! shard replaces whole, and reading it copies a pointer.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use mumble_server_runtime_crypto::CryptState;
use mumble_server_runtime_shard::{AudioRouting, ConnectionId, OutboundQueue, SessionId, ShardId};
use tokio::sync::watch;

use crate::limits::{TextBudget, VoiceBudget};

/// Which shard a connection currently belongs to, and where its routes come
/// from.
///
/// Replaced wholesale by a migration. The address, the key and the session are
/// deliberately *not* in here: a migration must not disturb the UDP plane, and
/// the surest way to guarantee that is for the fields it rewrites to be
/// disjoint from the fields the UDP plane depends on.
#[derive(Debug, Clone)]
pub struct ShardPlane {
    pub shard: ShardId,
    pub routing: watch::Receiver<Arc<AudioRouting>>,
}

/// One connection, as the voice plane sees it.
pub struct Peer {
    connection: ConnectionId,
    /// Stable for the life of the connection, across migrations included.
    session: SessionId,
    /// The TCP peer's host address. The cold path uses it to narrow the
    /// candidates for an unknown datagram.
    host: IpAddr,
    crypt: Mutex<CryptState>,
    /// The address this connection has proven it owns, if any.
    address: Mutex<Option<SocketAddr>>,
    /// Murmur's `aiUdpFlag`: whether this peer's own audio last arrived over
    /// UDP. It decides how *it* is reached, never how anyone else is.
    udp_mode: AtomicBool,
    budget: Mutex<VoiceBudget>,
    /// What this connection may still type. Its own bucket rather than a share
    /// of the voice one: a talkative user is not a flooding one.
    text: Mutex<TextBudget>,
    /// How far through its shard's journal the connection has been advanced.
    /// Written by the shard, read here (guide 9.5).
    cursor: Arc<AtomicU64>,
    queue: Arc<OutboundQueue>,
    plane: RwLock<ShardPlane>,
    /// When this connection was registered, for the one statistic it may be
    /// told about itself.
    online_since: Instant,
    /// The last numbers the client reported about its own side of the link.
    reported: Mutex<ClientReport>,
}

/// What a client last told us about the connection, in its own `Ping`.
///
/// Every field is the client's claim, not a measurement of ours: the reference
/// server stores them verbatim and hands them back in `UserStats`, which is what
/// fills the "To Client" column and the ping statistics of the information
/// window. Client-supplied numbers are only ever shown back to the client that
/// supplied them, so a client that lies here lies to itself alone.
///
/// REF: references/mumble/src/murmur/Messages.cpp : `msgPing` assigns
///   `uiRemoteGood/Late/Lost/Resync`, `dUDPPingAvg/Var`, `uiUDPPackets`,
///   `dTCPPingAvg/Var` and `uiTCPPackets` straight from the message.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ClientReport {
    pub good: u32,
    pub late: u32,
    pub lost: u32,
    pub resync: u32,
    pub udp_packets: u32,
    pub tcp_packets: u32,
    pub udp_ping_avg: f32,
    pub udp_ping_var: f32,
    pub tcp_ping_avg: f32,
    pub tcp_ping_var: f32,
}

/// Hand-written so key material never reaches a log. Everything printed here is
/// an identifier or a state flag; the OCB2 domain is named, not shown.
impl std::fmt::Debug for Peer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Peer")
            .field("connection", &self.connection)
            .field("session", &self.session)
            .field("host", &self.host)
            .field("address", &self.proven_address())
            .field("udp_mode", &self.udp_mode.load(Ordering::Relaxed))
            .field("shard", &self.shard())
            .field("cursor", &self.cursor())
            .finish_non_exhaustive()
    }
}

impl Peer {
    #[must_use]
    pub fn new(
        connection: ConnectionId,
        session: SessionId,
        host: IpAddr,
        crypt: CryptState,
        queue: Arc<OutboundQueue>,
        plane: ShardPlane,
        now: Instant,
    ) -> Peer {
        Peer {
            connection,
            session,
            host,
            crypt: Mutex::new(crypt),
            address: Mutex::new(None),
            udp_mode: AtomicBool::new(true),
            budget: Mutex::new(VoiceBudget::new(now)),
            text: Mutex::new(TextBudget::new(now)),
            // Created here rather than handed in: the shard writes it and the
            // voice plane reads it, and neither of them exists yet.
            cursor: Arc::new(AtomicU64::new(0)),
            queue,
            plane: RwLock::new(plane),
            online_since: now,
            reported: Mutex::new(ClientReport::default()),
        }
    }

    /// When this connection was registered.
    #[must_use]
    pub fn online_since(&self) -> Instant {
        self.online_since
    }

    /// Store what the client reported about its own side of the link.
    pub fn record_report(&self, report: ClientReport) {
        *lock(&self.reported) = report;
    }

    #[must_use]
    pub fn reported(&self) -> ClientReport {
        *lock(&self.reported)
    }

    /// Voice throughput over the last second, in bytes per second, and how long
    /// this connection has been idle.
    #[must_use]
    pub fn traffic(&self, now: Instant) -> (u32, Duration) {
        let mut budget = lock(&self.budget);
        (budget.bandwidth(now), budget.idle(now))
    }

    /// Note a control message that is not a keepalive.
    pub fn record_activity(&self, now: Instant) {
        lock(&self.budget).touch(now);
    }

    #[must_use]
    pub fn connection(&self) -> ConnectionId {
        self.connection
    }

    #[must_use]
    pub fn session(&self) -> SessionId {
        self.session
    }

    #[must_use]
    pub fn host(&self) -> IpAddr {
        self.host
    }

    #[must_use]
    pub fn queue(&self) -> Arc<OutboundQueue> {
        Arc::clone(&self.queue)
    }

    #[must_use]
    pub fn cursor_cell(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.cursor)
    }

    #[must_use]
    pub fn cursor(&self) -> u64 {
        self.cursor.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn shard(&self) -> ShardId {
        read(&self.plane).shard
    }

    /// The routing table of the shard this connection belongs to.
    #[must_use]
    pub fn routing(&self) -> Arc<AudioRouting> {
        let plane = read(&self.plane);
        Arc::clone(&plane.routing.borrow())
    }

    /// Point this connection at another shard. Called by a migration, once.
    pub fn move_to(&self, plane: ShardPlane) {
        *write(&self.plane) = plane;
    }

    /// Try to decrypt a datagram in this peer's OCB2 domain.
    ///
    /// A failure is side-effect free - the IV is restored and nothing is written
    /// to the replay history - which is what makes the cold path's "try every
    /// candidate" safe.
    ///
    /// REF: references/mumble/src/murmur/Server.cpp : `Server::run` binds an
    ///   unknown peer to the first `checkDecrypt` that succeeds.
    #[must_use]
    pub fn decrypt(&self, datagram: &[u8]) -> Option<Vec<u8>> {
        lock(&self.crypt).decrypt(datagram)
    }

    #[must_use]
    pub fn encrypt(&self, plaintext: &[u8]) -> Option<Vec<u8>> {
        lock(&self.crypt).encrypt(plaintext)
    }

    /// The OCB2 counters the TCP `Ping` reply must report.
    #[must_use]
    pub fn crypt_counters(&self) -> (u32, u32, u32) {
        let state = lock(&self.crypt);
        (state.good, state.late, state.lost)
    }

    /// Whether this peer may send one more voice packet right now.
    #[must_use]
    pub fn allow_voice(&self, now: Instant, bytes: usize) -> bool {
        lock(&self.budget).allow(now, bytes)
    }

    /// Whether this peer may send one more text message right now.
    #[must_use]
    pub fn allow_text(&self, now: Instant) -> bool {
        lock(&self.text).allow(now)
    }

    /// Record that this peer's audio is arriving over UDP again, or that it has
    /// fallen back to the tunnel.
    ///
    /// REF: references/mumble/src/murmur/Server.cpp : `aiUdpFlag` goes to 0 on a
    ///   `UDPTunnel` message and back to 1 when a datagram arrives.
    pub fn set_udp_mode(&self, on: bool) {
        self.udp_mode.store(on, Ordering::Relaxed);
    }

    /// Where audio for this peer goes: its proven address, or `None` meaning the
    /// TCP tunnel.
    ///
    /// Both conditions are required, as in the real server: a proven address
    /// *and* a peer whose own audio last came over UDP. A client whose UDP dies
    /// mid-call keeps hearing everyone, through the tunnel.
    #[must_use]
    pub fn destination(&self) -> Option<SocketAddr> {
        if !self.udp_mode.load(Ordering::Relaxed) {
            return None;
        }
        *lock(&self.address)
    }

    /// The address this peer has proven, whatever transport it last used.
    #[must_use]
    pub fn proven_address(&self) -> Option<SocketAddr> {
        *lock(&self.address)
    }

    fn bind(&self, address: SocketAddr) {
        *lock(&self.address) = Some(address);
    }

    /// Mark the connection for teardown by its own task.
    pub fn close(&self) {
        self.queue.mark_fatal();
    }
}

/// Every live connection, indexed the four ways the runtime needs.
///
/// Registration and removal are explicit rather than RAII on [`Peer`], because
/// the voice plane holds `Arc<Peer>` clones across `.await` points: a peer must
/// stop being *findable* the moment its connection ends, while the last
/// in-flight packet is allowed to finish with the copy it already has.
#[derive(Debug, Default)]
pub struct Peers {
    by_connection: RwLock<HashMap<ConnectionId, Arc<Peer>>>,
    by_session: RwLock<HashMap<SessionId, Arc<Peer>>>,
    by_address: RwLock<HashMap<SocketAddr, Arc<Peer>>>,
    /// The guide's runtime-global "host IP to connections" index. It cannot be
    /// per shard: an unknown datagram has not told us which shard it came from
    /// yet, which is the whole reason the cold path exists.
    by_host: RwLock<HashMap<IpAddr, Vec<Arc<Peer>>>>,
}

impl Peers {
    #[must_use]
    pub fn new() -> Peers {
        Peers::default()
    }

    pub fn insert(&self, peer: Arc<Peer>) {
        write(&self.by_connection).insert(peer.connection(), Arc::clone(&peer));
        write(&self.by_session).insert(peer.session(), Arc::clone(&peer));
        write(&self.by_host)
            .entry(peer.host())
            .or_default()
            .push(peer);
    }

    /// Forget a connection. Any address it had proven is released with it, so a
    /// later datagram from that address is re-proven rather than delivered to
    /// whoever inherits the socket.
    pub fn remove(&self, connection: ConnectionId) -> Option<Arc<Peer>> {
        let peer = write(&self.by_connection).remove(&connection)?;
        write(&self.by_session).remove(&peer.session());
        if let Some(address) = peer.proven_address() {
            write(&self.by_address).remove(&address);
        }
        let mut hosts = write(&self.by_host);
        if let Some(list) = hosts.get_mut(&peer.host()) {
            list.retain(|other| other.connection() != connection);
            if list.is_empty() {
                hosts.remove(&peer.host());
            }
        }
        Some(peer)
    }

    #[must_use]
    pub fn by_connection(&self, connection: ConnectionId) -> Option<Arc<Peer>> {
        read(&self.by_connection).get(&connection).cloned()
    }

    #[must_use]
    pub fn by_session(&self, session: SessionId) -> Option<Arc<Peer>> {
        read(&self.by_session).get(&session).cloned()
    }

    #[must_use]
    pub fn by_address(&self, address: SocketAddr) -> Option<Arc<Peer>> {
        read(&self.by_address).get(&address).cloned()
    }

    /// The connections whose TCP peer shares this host address.
    #[must_use]
    pub fn candidates(&self, host: IpAddr) -> Vec<Arc<Peer>> {
        read(&self.by_host).get(&host).cloned().unwrap_or_default()
    }

    /// Bind a proven address to a peer.
    ///
    /// The address is rebound, not merged: a client behind a NAT that reassigns
    /// its port keeps working, and the old entry is dropped so it cannot deliver
    /// to a peer that has moved.
    pub fn bind(&self, address: SocketAddr, peer: &Arc<Peer>) {
        if let Some(previous) = peer.proven_address()
            && previous != address
        {
            write(&self.by_address).remove(&previous);
        }
        peer.bind(address);
        write(&self.by_address).insert(address, Arc::clone(peer));
    }

    /// Every live connection attached to one shard.
    #[must_use]
    pub fn on_shard(&self, shard: ShardId) -> Vec<Arc<Peer>> {
        read(&self.by_connection)
            .values()
            .filter(|peer| peer.shard() == shard)
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        read(&self.by_connection).len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A poisoned lock means a panic unwound while a field was being read or
/// written. Nothing guarded here can be left half-updated - each guard covers a
/// single assignment or a single crypto call - so recovering is correct, whereas
/// propagating would take the whole voice plane down over one connection.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use mumble_server_runtime_shard::AudioRouting;

    fn plane() -> ShardPlane {
        let (_sender, routing) = watch::channel(Arc::new(AudioRouting::default()));
        ShardPlane {
            shard: ShardId(1),
            routing,
        }
    }

    fn peer(connection: u64, session: u32, host: [u8; 4]) -> Arc<Peer> {
        // Dropping the writer half closes the queue, which is harmless here:
        // these tests exercise the registry and never push a message.
        let (queue, _writer) = OutboundQueue::new();
        Arc::new(Peer::new(
            ConnectionId(connection),
            SessionId(session),
            IpAddr::from(host),
            CryptState::new(&[0u8; 16], &[0u8; 16], &[1u8; 16]),
            Arc::new(queue),
            plane(),
            Instant::now(),
        ))
    }

    fn address(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    #[test]
    fn a_removed_connection_releases_its_address() {
        let peers = Peers::new();
        let alice = peer(1, 10, [127, 0, 0, 1]);
        peers.insert(Arc::clone(&alice));
        peers.bind(address(5000), &alice);

        assert!(peers.by_address(address(5000)).is_some());
        peers.remove(ConnectionId(1));

        assert!(
            peers.by_address(address(5000)).is_none(),
            "an address left bound would deliver to whoever inherits the socket"
        );
        assert!(peers.by_session(SessionId(10)).is_none());
        assert!(peers.candidates(IpAddr::from([127, 0, 0, 1])).is_empty());
    }

    #[test]
    fn rebinding_an_address_drops_the_previous_one() {
        let peers = Peers::new();
        let alice = peer(1, 10, [127, 0, 0, 1]);
        peers.insert(Arc::clone(&alice));

        peers.bind(address(5000), &alice);
        peers.bind(address(5001), &alice);

        assert!(peers.by_address(address(5000)).is_none());
        assert_eq!(
            peers
                .by_address(address(5001))
                .map(|found| found.connection()),
            Some(ConnectionId(1))
        );
    }

    #[test]
    fn the_host_index_narrows_the_cold_path() {
        let peers = Peers::new();
        peers.insert(peer(1, 10, [10, 0, 0, 1]));
        peers.insert(peer(2, 20, [10, 0, 0, 1]));
        peers.insert(peer(3, 30, [10, 0, 0, 2]));

        assert_eq!(peers.candidates(IpAddr::from([10, 0, 0, 1])).len(), 2);
        assert_eq!(peers.candidates(IpAddr::from([10, 0, 0, 2])).len(), 1);
        assert!(peers.candidates(IpAddr::from([10, 0, 0, 3])).is_empty());
    }

    #[test]
    fn a_peer_on_the_tunnel_has_no_udp_destination() {
        let peers = Peers::new();
        let alice = peer(1, 10, [127, 0, 0, 1]);
        peers.insert(Arc::clone(&alice));
        peers.bind(address(5000), &alice);
        assert_eq!(alice.destination(), Some(address(5000)));

        alice.set_udp_mode(false);
        assert_eq!(
            alice.destination(),
            None,
            "a client that tunnelled its own audio must be answered on the tunnel"
        );
        assert_eq!(
            alice.proven_address(),
            Some(address(5000)),
            "falling back must not forget the proof, only stop using it"
        );
    }
}
