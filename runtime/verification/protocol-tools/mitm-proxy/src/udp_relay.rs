//! Async UDP voice-plane relay: the socket wiring that carries a live client's
//! voice through the proxy (Phase 2, tranche T4).
//!
//! [`crate::udp`] holds the pure per-datagram re-encryption; this module is the
//! IO around it. It owns one client-facing socket, correlates each datagram's
//! source address to the [`Session`] that holds the matching cipher domains, and
//! gives every correlated client its own upstream socket so the real server can
//! tell clients apart (mirroring the Phase 0 recording proxy).
//!
//! Address -> session correlation follows the real server:
//! REF: runtime/references/mumble/src/murmur/Server.cpp : `Server::run` UDP loop — a
//!      datagram from a known peer address uses that peer's `CryptState`; from an
//!      unknown peer the server loops the users sharing the same host IP
//!      (`qhHostUsers`) and binds the address on the first `checkDecrypt` success
//!      (`qhPeerUsers.insert`). Unencrypted connectivity pings are answered before
//!      any association is attempted.
//! REF: runtime/references/mumble/src/murmur/Server.cpp : `Server::checkDecrypt` — a
//!      failed decrypt is side-effect-free (OCB2 `decrypt` restores its IV and
//!      writes no replay history on failure, see mumble-server-runtime-crypto), so trying one
//!      datagram against several candidate domains cannot corrupt them. That is
//!      what makes the "bind on first success" loop safe here too.
//!
//! Every datagram ends in one explicit outcome — re-encrypted and forwarded,
//! passed through (a ping), or dropped with a logged reason — never a silent
//! discard (L4).

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex as StdMutex, PoisonError};

use anyhow::{Context, Result};
use tokio::net::UdpSocket;

use crate::relay::Origin;
use crate::session::Session;
use crate::udp::{UdpOutcome, is_connectivity_ping, reencrypt_from_client, reencrypt_from_server};

const UDP_BUFFER_SIZE: usize = 64 * 1024;

/// A session shared between its TCP control task (which builds the cipher domains
/// and applies mid-session re-keys) and the UDP relay (which uses them).
pub type SharedSession = Arc<StdMutex<Session>>;

/// Active sessions indexed by the client host IP they connected from, so the UDP
/// relay can find the cipher domains for a datagram's source address. Mirrors
/// Murmur's `qhHostUsers`. Cloneable: the TCP relay and the UDP relay share one.
#[derive(Clone, Default)]
pub struct Registry {
    hosts: Arc<StdMutex<HashMap<IpAddr, Vec<SharedSession>>>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `session` under `ip` for the lifetime of the returned guard. The
    /// TCP connection task holds the guard; dropping it (connection closed)
    /// deregisters the session so a stale domain never binds a later datagram.
    pub fn register(&self, ip: IpAddr, session: SharedSession) -> Registration {
        self.lock_hosts()
            .entry(ip)
            .or_default()
            .push(Arc::clone(&session));
        Registration {
            registry: self.clone(),
            ip,
            session,
        }
    }

    /// Candidate sessions whose client connected from `ip`.
    fn candidates(&self, ip: IpAddr) -> Vec<SharedSession> {
        self.lock_hosts().get(&ip).cloned().unwrap_or_default()
    }

    /// Recover the guard even across a poisoned lock: this is a best-effort
    /// research tool, and a poisoned registry must not panic the relay loop.
    fn lock_hosts(&self) -> std::sync::MutexGuard<'_, HashMap<IpAddr, Vec<SharedSession>>> {
        self.hosts.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// RAII deregistration: removing a session on drop is what keeps the host index
/// in step with live connections.
pub struct Registration {
    registry: Registry,
    ip: IpAddr,
    session: SharedSession,
}

impl Drop for Registration {
    fn drop(&mut self) {
        let mut hosts = self.registry.lock_hosts();
        if let Some(list) = hosts.get_mut(&self.ip) {
            list.retain(|existing| !Arc::ptr_eq(existing, &self.session));
            if list.is_empty() {
                hosts.remove(&self.ip);
            }
        }
    }
}

/// One client's upstream leg: a socket connected to the real server, plus the
/// session (once correlated) whose domains re-encrypt this client's voice. The
/// session is filled in lazily by the first voice datagram that decrypts.
struct Peer {
    uplink: Arc<UdpSocket>,
    session: Arc<StdMutex<Option<SharedSession>>>,
}

/// Bind the client-facing UDP socket and relay voice until a fatal socket error.
pub async fn serve_udp(listen: SocketAddr, upstream: SocketAddr, registry: Registry) -> Result<()> {
    let client_facing = Arc::new(
        UdpSocket::bind(listen)
            .await
            .with_context(|| format!("binding UDP voice socket on {listen}"))?,
    );
    eprintln!("MITM UDP voice relay on {listen} -> {upstream}");
    run_udp(client_facing, upstream, registry).await
}

/// The relay loop over an already-bound client-facing socket. Split out so an
/// integration test can drive it against loopback sockets whose ports it chose.
pub async fn run_udp(
    client_facing: Arc<UdpSocket>,
    upstream: SocketAddr,
    registry: Registry,
) -> Result<()> {
    let mut peers: HashMap<SocketAddr, Peer> = HashMap::new();
    let mut buffer = vec![0u8; UDP_BUFFER_SIZE];

    loop {
        let (read, from) = client_facing
            .recv_from(&mut buffer)
            .await
            .context("receiving UDP datagram")?;
        let datagram = &buffer[..read];

        let peer = match peers.entry(from) {
            Entry::Occupied(existing) => existing.into_mut(),
            Entry::Vacant(slot) => {
                let uplink = match new_uplink(upstream).await {
                    Ok(socket) => socket,
                    Err(error) => {
                        eprintln!("UDP uplink for {from} failed: {error:#}");
                        continue;
                    }
                };
                let session = Arc::new(StdMutex::new(None));
                spawn_uplink_reader(
                    Arc::clone(&uplink),
                    Arc::clone(&client_facing),
                    from,
                    Arc::clone(&session),
                );
                slot.insert(Peer { uplink, session })
            }
        };

        if let Some(bytes) = client_to_server_bytes(&registry, peer, from, datagram)
            && let Err(error) = peer.uplink.send(&bytes).await
        {
            eprintln!("forwarding UDP from {from} failed: {error:#}");
        }
    }
}

/// Decide what leaves the proxy toward the server for one client datagram, and
/// bind the source address to a session the first time voice decrypts.
fn client_to_server_bytes(
    registry: &Registry,
    peer: &Peer,
    from: SocketAddr,
    datagram: &[u8],
) -> Option<Vec<u8>> {
    // Already bound: re-encrypt through the known session's domains.
    if let Some(shared) = lock_session(&peer.session) {
        return reencrypt_leg(&shared, Origin::Client, datagram, from);
    }

    // Unbound. A connectivity ping forwards verbatim and does not bind: a ping is
    // not proof of identity, exactly as the server answers pings before it has
    // associated an address (REF: Server::run, `handlePing` precedes association).
    if is_connectivity_ping(Origin::Client, datagram) {
        return Some(datagram.to_vec());
    }

    // Otherwise associate: the first candidate whose domain decrypts this datagram
    // owns the address, and the same call yields the bytes to forward — decrypting
    // twice would replay-reject the second read, so bind and forward as one step.
    for candidate in registry.candidates(from.ip()) {
        let outcome = {
            let mut guard = candidate.lock().unwrap_or_else(PoisonError::into_inner);
            match guard.channels_mut() {
                Some(channels) => reencrypt_from_client(channels, datagram),
                // No cipher domains yet (CryptSetup not seen): not a match.
                None => continue,
            }
        };
        match outcome {
            UdpOutcome::Reencrypted(bytes) => {
                *peer.session.lock().unwrap_or_else(PoisonError::into_inner) = Some(candidate);
                return Some(bytes);
            }
            // A ping slipped past the check above (shape-only match): forward it,
            // still without binding.
            UdpOutcome::PassThrough => return Some(datagram.to_vec()),
            // Wrong candidate (or undecodable under it): try the next one.
            UdpOutcome::Dropped(_) => continue,
        }
    }

    eprintln!("UDP from {from} matched no session; dropping");
    None
}

/// Spawn the reader for one client's upstream socket: it re-encrypts each server
/// datagram toward that client and returns it on the shared client-facing socket.
fn spawn_uplink_reader(
    uplink: Arc<UdpSocket>,
    client_facing: Arc<UdpSocket>,
    client_addr: SocketAddr,
    session: Arc<StdMutex<Option<SharedSession>>>,
) {
    // Detached on purpose: its lifetime is this client's UDP flow, it holds no
    // resource needing orderly shutdown, and it dies when the process exits at
    // Ctrl-C. Mirrors the Phase 0 recording proxy's uplink readers.
    tokio::spawn(async move {
        let mut buffer = vec![0u8; UDP_BUFFER_SIZE];
        loop {
            let read = match uplink.recv(&mut buffer).await {
                Ok(read) => read,
                Err(error) => {
                    eprintln!("UDP uplink for {client_addr} closed: {error:#}");
                    break;
                }
            };
            let datagram = &buffer[..read];

            let bytes = if is_connectivity_ping(Origin::Server, datagram) {
                Some(datagram.to_vec())
            } else {
                match lock_session(&session) {
                    Some(shared) => reencrypt_leg(&shared, Origin::Server, datagram, client_addr),
                    // Server voice before this client is bound: the proxy has no
                    // domain to re-encrypt it with. Drop rather than leak the real
                    // server's ciphertext to the client (fail closed).
                    None => {
                        eprintln!(
                            "server->client datagram for {client_addr} before binding; dropping"
                        );
                        None
                    }
                }
            };

            if let Some(bytes) = bytes
                && let Err(error) = client_facing.send_to(&bytes, client_addr).await
            {
                eprintln!("returning UDP to {client_addr} failed: {error:#}");
                break;
            }
        }
    });
}

/// Re-encrypt one datagram through a bound session for the given travel
/// direction, logging and dropping (returning `None`) on any non-forward outcome.
fn reencrypt_leg(
    shared: &SharedSession,
    origin: Origin,
    datagram: &[u8],
    peer: SocketAddr,
) -> Option<Vec<u8>> {
    let mut guard = shared.lock().unwrap_or_else(PoisonError::into_inner);
    let channels = guard.channels_mut()?;
    let outcome = match origin {
        Origin::Client => reencrypt_from_client(channels, datagram),
        Origin::Server => reencrypt_from_server(channels, datagram),
    };
    match outcome {
        UdpOutcome::Reencrypted(bytes) => Some(bytes),
        UdpOutcome::PassThrough => Some(datagram.to_vec()),
        UdpOutcome::Dropped(reason) => {
            eprintln!("dropping {origin:?} datagram for {peer}: {reason:?}");
            None
        }
    }
}

/// Clone the bound session out from behind its lock, holding that lock no longer
/// than the clone. Keeping it short avoids ever holding the slot lock and a
/// session lock at the same time, which is what keeps the relay deadlock-free.
fn lock_session(slot: &Arc<StdMutex<Option<SharedSession>>>) -> Option<SharedSession> {
    slot.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// A fresh UDP socket connected to `upstream`, one per client so the real server
/// sees distinct source ports. REF: recording-proxy `new_uplink`.
async fn new_uplink(upstream: SocketAddr) -> Result<Arc<UdpSocket>> {
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .context("binding UDP uplink socket")?;
    socket
        .connect(upstream)
        .await
        .with_context(|| format!("connecting UDP uplink to {upstream}"))?;
    Ok(Arc::new(socket))
}
