//! Several shards, one runtime, and the only operation that touches two shards
//! at once.
//!
//! Shards are independent by design: each owns a task, a view, a journal and a
//! routing table, and nothing here coordinates their turns. What the runtime
//! adds is the small amount of shared state that genuinely cannot be per shard -
//! the identifier allocator, the connection registry, the shard directory - plus
//! the one interaction that spans two of them, [`RuntimeHandle::move_connection`].
//!
//! REF: docs/design/guide-implementation.md 9.6, 10

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};
use mumble_server_runtime_shard::{
    ConnectionId, Handover, Occupant, SessionId, Shard, ShardCommand, ShardHandle, ShardId,
    ShardLogic, SharedIds,
};

use crate::peer::{Peer, Peers, ShardPlane};

/// How many runtime commands queue before a caller is refused.
const MAILBOX_DEPTH: usize = 256;

/// What an operator can see about one shard without touching its task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardStatus {
    pub shard: ShardId,
    pub connections: usize,
    /// The furthest any attached connection is behind the others, in journal
    /// versions. The guide calls this the best single health indicator, and it
    /// is: a connection that stops advancing is one that has stopped keeping up.
    pub worst_lag: u64,
}

/// A command only the runtime supervisor may carry out.
#[derive(Debug)]
enum RuntimeCommand {
    Move {
        connection: ConnectionId,
        to: ShardId,
    },
    Destroy {
        shard: ShardId,
        reason: String,
    },
}

/// One shard, as the runtime tracks it.
struct ShardEntry {
    handle: ShardHandle,
    plane: ShardPlane,
    /// Owning the task is what makes destruction real: dropping the entry
    /// aborts it. A detached task would keep rendering a shard nobody can reach.
    task: JoinHandle<()>,
}

struct RuntimeInner {
    ids: SharedIds,
    peers: Arc<Peers>,
    shards: RwLock<HashMap<ShardId, ShardEntry>>,
    next_shard: AtomicU64,
    next_connection: AtomicU64,
    commands: mpsc::Sender<RuntimeCommand>,
}

/// The runtime, owned by whoever started it.
///
/// Holding it keeps the supervisor task alive; dropping it ends the supervisor
/// and, with it, every shard.
pub struct Runtime {
    handle: RuntimeHandle,
    supervisor: JoinHandle<()>,
}

impl Runtime {
    /// Build a runtime and start its supervisor.
    #[must_use]
    pub fn start() -> Runtime {
        let (commands, mailbox) = mpsc::channel(MAILBOX_DEPTH);
        let inner = Arc::new(RuntimeInner {
            ids: SharedIds::new(),
            peers: Arc::new(Peers::new()),
            shards: RwLock::new(HashMap::new()),
            next_shard: AtomicU64::new(1),
            next_connection: AtomicU64::new(1),
            commands,
        });
        let supervisor = tokio::spawn(supervise(Arc::clone(&inner), mailbox));
        Runtime {
            handle: RuntimeHandle { inner },
            supervisor,
        }
    }

    #[must_use]
    pub fn handle(&self) -> RuntimeHandle {
        self.handle.clone()
    }

    /// Stop the supervisor and every shard.
    pub fn shutdown(self) {
        self.supervisor.abort();
        write(&self.handle.inner.shards).clear();
    }
}

/// A cheap clone of the runtime, safe to hand to flavors and connection tasks.
#[derive(Clone)]
pub struct RuntimeHandle {
    inner: Arc<RuntimeInner>,
}

impl std::fmt::Debug for RuntimeHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeHandle")
            .field("shards", &read(&self.inner.shards).len())
            .field("connections", &self.inner.peers.len())
            .finish()
    }
}

impl RuntimeHandle {
    /// The runtime-wide identifier allocator.
    ///
    /// Public because the gateway needs a connection's session before any shard
    /// has rendered it: `ServerSync` carries it, and the handshake happens
    /// before the first turn.
    #[must_use]
    pub fn ids(&self) -> &SharedIds {
        &self.inner.ids
    }

    #[must_use]
    pub fn peers(&self) -> &Arc<Peers> {
        &self.inner.peers
    }

    /// Reserve the next connection identifier. Never reused.
    #[must_use]
    pub fn next_connection(&self) -> ConnectionId {
        ConnectionId(self.inner.next_connection.fetch_add(1, Ordering::Relaxed))
    }

    /// The session a connection will be rendered under, allocated now so the
    /// handshake can announce it.
    ///
    /// # Errors
    ///
    /// Once the session space is spent.
    pub fn session_for(
        &self,
        connection: ConnectionId,
    ) -> Result<SessionId, mumble_server_runtime_shard::Exhausted> {
        self.inner.ids.session(Occupant::Connection(connection))
    }

    /// Create a shard and start its task.
    ///
    /// The chicken and the egg - a flavor usually wants to wake the shard that
    /// owns it - are resolved by a closure: the handle exists before the logic
    /// that will hold it.
    pub fn create_shard<L: ShardLogic>(&self, build: impl FnOnce(ShardHandle) -> L) -> ShardHandle {
        let id = ShardId(self.inner.next_shard.fetch_add(1, Ordering::Relaxed));
        let (handle, wake, mailbox) = mumble_server_runtime_shard::spawn_parts(id);
        let logic = build(handle.clone());
        let mut shard = Shard::with_ids(id, logic, self.inner.ids.clone());
        shard.route_effects(self.effects());
        let routing = shard.routing();
        let task = tokio::spawn(async move {
            let _shard = mumble_server_runtime_shard::run(shard, wake, mailbox).await;
        });

        write(&self.inner.shards).insert(
            id,
            ShardEntry {
                handle: handle.clone(),
                plane: ShardPlane { shard: id, routing },
                task,
            },
        );
        handle
    }

    /// Destroy a shard.
    ///
    /// The guide leaves the policy for its connections open (17.4). The safe
    /// default, and what this does, is to close them: a connection left pointing
    /// at a shard that no longer renders would hold a view nothing can ever
    /// update. A flavor that wants a fallback shard migrates them first and
    /// destroys afterwards.
    pub fn destroy_shard(&self, shard: ShardId, reason: &str) {
        let refused = self.inner.commands.try_send(RuntimeCommand::Destroy {
            shard,
            reason: reason.to_owned(),
        });
        if let Err(error) = refused {
            eprintln!("mumble-server-runtime-gateway: cannot destroy shard {shard:?}: {error}");
        }
    }

    /// What a shard hands back when its flavor asks for something only the
    /// runtime can do.
    ///
    /// Weak on purpose. The closure lives inside the shard, the shard inside its
    /// task, and the task inside this runtime's directory: holding a strong
    /// reference here would close that ring, and the runtime would outlive every
    /// handle to it forever. An effect arriving after the runtime is gone has
    /// nothing left to act on, which is exactly what a failed upgrade says.
    fn effects(&self) -> mumble_server_runtime_shard::Effects {
        let runtime = Arc::downgrade(&self.inner);
        Arc::new(move |effect| {
            let Some(inner) = runtime.upgrade() else {
                return;
            };
            match effect {
                mumble_server_runtime_shard::Effect::Move { connection, to } => {
                    RuntimeHandle { inner }.move_connection(connection, to);
                }
                // The vocabulary is non-exhaustive: a shard that starts asking
                // for something new must not have it silently ignored.
                other => eprintln!("mumble-server-runtime-gateway: no runtime support for {other:?}"),
            }
        })
    }

    /// Move a connection to another shard.
    ///
    /// Not a disconnect followed by a connect. The source shard hands over the
    /// view the client still holds, and the destination plans **one** transition
    /// from it onto its own tree, which is the same slow path a scope change
    /// takes: whatever the two trees have in common does not flicker.
    ///
    /// Doing it the other way round is not merely wasteful, it disconnects the
    /// official client. It keeps itself in its own model after a `UserRemove`
    /// naming its own session, so the `ChannelRemove` that follows looks to it
    /// like the server removing an occupied channel, which it treats as a
    /// protocol violation.
    ///
    /// REF: references/mumble/src/mumble/Messages.cpp : `msgUserRemove` calls
    ///   `removeUser` only `if (pDst != pSelf)`; `msgChannelRemove` logs
    ///   "Protocol violation. Server sent remove for occupied channel." and
    ///   disconnects when `UserModel::removeChannel(c, true)` refuses.
    pub fn move_connection(&self, connection: ConnectionId, to: ShardId) {
        let refused = self
            .inner
            .commands
            .try_send(RuntimeCommand::Move { connection, to });
        if let Err(error) = refused {
            eprintln!("mumble-server-runtime-gateway: cannot move {connection:?} to {to:?}: {error}");
        }
    }

    /// Attach a connection that has just been routed.
    ///
    /// Returns the receiver that fires once the shard has accepted the
    /// connection's first transition, which is what the handshake waits on
    /// before sending `ServerSync`.
    ///
    /// # Errors
    ///
    /// When the shard does not exist, or its mailbox is full.
    pub fn attach(
        &self,
        peer: &Arc<Peer>,
        shard: ShardId,
    ) -> Result<oneshot::Receiver<()>, AttachError> {
        let plane = self.plane(shard).ok_or(AttachError::NoSuchShard(shard))?;
        let handle = self
            .shard_handle(shard)
            .ok_or(AttachError::NoSuchShard(shard))?;
        peer.move_to(plane);

        let (ready, awaited) = oneshot::channel();
        handle
            .send(ShardCommand::Attach {
                connection: peer.connection(),
                queue: peer.queue(),
                cursor: peer.cursor_cell(),
                held: mumble_server_runtime_shard::ShardView::empty(),
                ready: Some(ready),
            })
            .map_err(|_full| AttachError::Unreachable(shard))?;
        Ok(awaited)
    }

    /// Tell a connection's shard that it is gone.
    pub fn detach(&self, connection: ConnectionId, shard: ShardId, reason: &str) {
        let Some(handle) = self.shard_handle(shard) else {
            return;
        };
        // A full mailbox here would leave the shard rendering a connection that
        // no longer exists, so it is reported rather than swallowed. The next
        // render still cannot reach it: its queue is closed.
        if let Err(error) = handle.send(ShardCommand::detach(connection, reason)) {
            eprintln!("mumble-server-runtime-gateway: shard {shard:?} did not take a detach: {error}");
        }
    }

    /// Forward a command to a shard.
    ///
    /// # Errors
    ///
    /// When the shard does not exist or its mailbox is full.
    pub fn send(&self, shard: ShardId, command: ShardCommand) -> Result<(), AttachError> {
        let handle = self
            .shard_handle(shard)
            .ok_or(AttachError::NoSuchShard(shard))?;
        handle
            .send(command)
            .map_err(|_full| AttachError::Unreachable(shard))?;
        Ok(())
    }

    /// What an operator would want on a status page.
    #[must_use]
    pub fn status(&self) -> Vec<ShardStatus> {
        let shards: Vec<ShardId> = read(&self.inner.shards).keys().copied().collect();
        shards
            .into_iter()
            .map(|shard| {
                let peers = self.inner.peers.on_shard(shard);
                let cursors: Vec<u64> = peers.iter().map(|peer| peer.cursor()).collect();
                let worst_lag = match (cursors.iter().min(), cursors.iter().max()) {
                    (Some(behind), Some(ahead)) => ahead.saturating_sub(*behind),
                    _ => 0,
                };
                ShardStatus {
                    shard,
                    connections: peers.len(),
                    worst_lag,
                }
            })
            .collect()
    }

    #[must_use]
    pub fn shard_handle(&self, shard: ShardId) -> Option<ShardHandle> {
        read(&self.inner.shards)
            .get(&shard)
            .map(|entry| entry.handle.clone())
    }

    #[must_use]
    fn plane(&self, shard: ShardId) -> Option<ShardPlane> {
        read(&self.inner.shards)
            .get(&shard)
            .map(|entry| entry.plane.clone())
    }
}

/// Why a connection could not reach a shard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AttachError {
    #[error("shard {0:?} does not exist")]
    NoSuchShard(ShardId),
    #[error("shard {0:?} is not taking commands")]
    Unreachable(ShardId),
}

/// The runtime's own task: the only place two shards are touched at once.
async fn supervise(inner: Arc<RuntimeInner>, mut mailbox: mpsc::Receiver<RuntimeCommand>) {
    // Migrations run concurrently and are owned: a migration that never
    // completes because a shard stopped answering is reaped when the set is
    // dropped, rather than left running unowned.
    let mut migrations: JoinSet<()> = JoinSet::new();

    loop {
        tokio::select! {
            // Cancellation-safe: `recv` takes a command only when it completes.
            command = mailbox.recv() => match command {
                Some(RuntimeCommand::Move { connection, to }) => {
                    let inner = Arc::clone(&inner);
                    migrations.spawn(migrate(inner, connection, to));
                }
                Some(RuntimeCommand::Destroy { shard, reason }) => destroy(&inner, shard, &reason),
                // Every handle is gone.
                None => break,
            },
            // Cancellation-safe: joining is idempotent, and a task not reaped on
            // this pass is reaped on the next. Disabled when empty so the branch
            // cannot resolve to `None` in a busy loop.
            Some(_finished) = migrations.join_next(), if !migrations.is_empty() => {}
        }
    }
}

/// Move one connection between two shards.
async fn migrate(inner: Arc<RuntimeInner>, connection: ConnectionId, to: ShardId) {
    let Some(peer) = inner.peers.by_connection(connection) else {
        // It disconnected while the command was in flight. Nothing to move.
        return;
    };
    let from = peer.shard();
    if from == to {
        return;
    }

    let (source, destination) = {
        let shards = read(&inner.shards);
        let source = shards.get(&from).map(|entry| entry.handle.clone());
        let destination = shards
            .get(&to)
            .map(|entry| (entry.handle.clone(), entry.plane.clone()));
        (source, destination)
    };
    let Some((destination, plane)) = destination else {
        eprintln!("mumble-server-runtime-gateway: {connection:?} cannot move to {to:?}: no such shard");
        return;
    };

    // The held view has to come from the source before the destination can plan
    // onto it. If the source is gone the client's view is unknown, so the
    // connection is closed rather than handed a transition planned from a guess.
    let held = match source {
        Some(source) => {
            let (view, awaited) = oneshot::channel();
            let sent = source.send(ShardCommand::Detach {
                connection,
                reason: format!("moving to {to:?}"),
                handover: Some(Handover { to, view }),
            });
            if sent.is_err() {
                peer.close();
                return;
            }
            match awaited.await {
                Ok(held) => held,
                Err(_dropped) => {
                    peer.close();
                    return;
                }
            }
        }
        None => {
            peer.close();
            return;
        }
    };

    // The routing table follows the connection before its view does: a route it
    // is no longer entitled to disappears immediately, and a route it gains is
    // inert until its cursor catches up (guide 9.5). Cutting early is always
    // safe; the reverse never is.
    peer.move_to(plane);

    let handed = destination.send(ShardCommand::Attach {
        connection,
        queue: peer.queue(),
        cursor: peer.cursor_cell(),
        held,
        ready: None,
    });
    if handed.is_err() {
        // Attached nowhere and holding a view no shard will ever update.
        eprintln!("mumble-server-runtime-gateway: {connection:?} could not be handed to {to:?}");
        peer.close();
    }
}

fn destroy(inner: &Arc<RuntimeInner>, shard: ShardId, reason: &str) {
    // Dropping the entry aborts the shard's task, so the connections have to be
    // dealt with first: after this, nothing can render them.
    for peer in inner.peers.on_shard(shard) {
        eprintln!(
            "mumble-server-runtime-gateway: closing {:?}: its shard was destroyed ({reason})",
            peer.connection()
        );
        peer.close();
    }
    write(&inner.shards).remove(&shard);
}

/// A poisoned directory means a panic unwound mid-update. The map is still
/// structurally sound, and refusing every later lookup would take the runtime
/// down over one thread.
fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

impl Drop for ShardEntry {
    fn drop(&mut self) {
        self.task.abort();
    }
}
