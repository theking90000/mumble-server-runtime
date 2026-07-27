//! The shard: one render, one journal, one task.
//!
//! ```text
//!    business state changes
//!         |  wake()
//!         v
//!   +------------------------------------------------------+
//!   | the shard's task (woken, one per shard)               |
//!   |                                                       |
//!   |  1. logic.render(&mut builder)                        |
//!   |       -> the SHARED view + the PRIVATE overlays        |
//!   |          + the AUDIO relation                          |
//!   |  2. ops = plan(current view -> new view)               |
//!   |  3. version += 1, ops -> journal                       |
//!   |  4. publish the audio routing table                    |
//!   |  5. for each connection:                               |
//!   |       its observation changed? -> replan for it        |
//!   |       otherwise -> filter, splice, collapse, encode,   |
//!   |                    push, advance its committed state   |
//!   +------------------------------------------------------+
//! ```
//!
//! # The rules this module must never break
//!
//! 1. A shard task never awaits IO.
//! 2. The audio routing table is published **before** the views are pushed. A
//!    revoked route disappears immediately, which is always safe: cutting too
//!    early means hearing less than your due, never more. A granted route is
//!    inert until the receiver's cursor catches up.
//! 3. The committed state of a connection is a **triplet** - cursor, observation,
//!    overlay - and the three only advance together, once the queue has accepted
//!    everything. This is what lets a lagging connection catch up naturally: the
//!    shared part replays from the journal, the private part is recomputed from
//!    `overlay_sent`, so an overlay never needs journalling.
//! 4. An invalid render never replaces the current view: log it, keep the old
//!    one, and do not close anything.
//!
//! REF: docs/design/guide-implementation.md 9

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::{Notify, mpsc, watch};

use crate::build::{BuildError, ShardBuilder};
use crate::compose::{collapse, filter};
use crate::emit::emit;
use crate::ids::{ConnectionId, IdAllocator, Occupant, SessionId, ShardId};
use crate::journal::Journal;
use crate::plan::{PlanOp, plan, plan_elements};
use crate::queue::{OutboundQueue, Refused};
use crate::routing::{AudioRelation, AudioRouting, compile};
use crate::scope::ScopeSet;
use crate::view::{Overlay, ShardView};

/// The floor between two renders.
///
/// A throughput guard, not a policy. There is **no tick** in the runtime:
/// Voxloom has no idea why a flavor would want a rhythm, so a flavor that wants
/// one runs its own timer and calls [`ShardHandle::wake`].
pub const MIN_INTERVAL: Duration = Duration::from_millis(50);

/// A voice-plane fact reported to the flavor.
///
/// Always a statement about the runtime, never a command: the flavor alone
/// decides what its state becomes, and ignoring an event is a valid answer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum VoiceEvent {
    /// A connection was attached to this shard.
    Connected { connection: ConnectionId },
    /// A connection is gone. No further event will carry it.
    Disconnected {
        connection: ConnectionId,
        reason: String,
    },
}

/// What a flavor writes.
///
/// `Send + 'static` and deliberately **not** `Sync`: the runtime never needs to
/// share the logic, so it imposes no synchronization. A concrete flavor may
/// happen to be `Sync` if it holds one, which is its own business.
pub trait ShardLogic: Send + 'static {
    /// Build the shared view, the private overlays and the audio relation.
    ///
    /// No viewer parameter: what is SHARED cannot depend on who is looking.
    fn render(&mut self, out: &mut ShardBuilder<'_>);

    /// What this connection observes of the shared view.
    ///
    /// Called once per connection per turn, so it **must** stay a small `Copy`
    /// value. Returning something bigger is how the cost goes quadratic again.
    fn observation(&mut self, connection: ConnectionId) -> ScopeSet;

    /// A voice fact happened. The flavor alone decides what to do with it.
    fn observe(&mut self, event: &VoiceEvent);
}

/// A command for a shard's task.
#[derive(Debug)]
pub enum ShardCommand {
    Attach {
        connection: ConnectionId,
        queue: OutboundQueue,
    },
    Detach {
        connection: ConnectionId,
        reason: String,
    },
    /// A connection's queue drained: retry for **that one alone**, in O(1).
    Drained(ConnectionId),
}

/// One connection attached to a shard, and its committed state.
#[derive(Debug)]
pub struct AttachedConnection {
    id: ConnectionId,
    session: SessionId,
    /// How far through the SHARED journal it has been advanced.
    cursor: u64,
    /// What it observes of the shared view.
    see: ScopeSet,
    /// What PRIVATE elements it has received.
    overlay_sent: Overlay,
    queue: OutboundQueue,
    /// Read by the voice plane to gate newly granted routes (guide 9.5).
    shared_cursor: Arc<AtomicU64>,
}

impl AttachedConnection {
    #[must_use]
    pub fn id(&self) -> ConnectionId {
        self.id
    }

    #[must_use]
    pub fn session(&self) -> SessionId {
        self.session
    }

    #[must_use]
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    #[must_use]
    pub fn observation(&self) -> ScopeSet {
        self.see
    }

    /// The private elements this connection has actually received.
    #[must_use]
    pub fn overlay_sent(&self) -> &Overlay {
        &self.overlay_sent
    }

    /// Whether the queue has refused something unrecoverable.
    #[must_use]
    pub fn must_close(&self) -> bool {
        self.queue.must_close()
    }

    /// The cursor cell the voice plane gates on.
    #[must_use]
    pub fn shared_cursor(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.shared_cursor)
    }

    /// Advance the committed triplet. The only place the three move, and they
    /// move together.
    fn commit(&mut self, cursor: u64, see: ScopeSet, overlay: Overlay) {
        self.cursor = cursor;
        self.see = see;
        self.overlay_sent = overlay;
        self.shared_cursor.store(cursor, Ordering::Relaxed);
    }
}

/// What one [`Shard::reconcile`] did. Everything an operator or a test needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// The version after this turn.
    pub version: u64,
    /// Whether a new version was published.
    pub published: bool,
    /// How many operations the shared delta held.
    pub delta_len: usize,
    /// Connections that took the slow path because their observation moved.
    pub replanned: Vec<ConnectionId>,
    /// Whether the audio routing table was recompiled.
    pub routing_recompiled: bool,
    /// Connections that must be torn down.
    pub closed: Vec<ConnectionId>,
    /// Set when the render was refused: the previous view was kept.
    pub refused: Option<BuildError>,
}

/// A shard: unit of ownership, scheduling, rendering, id allocation and audio
/// routing.
pub struct Shard<L: ShardLogic> {
    id: ShardId,
    logic: L,
    ids: IdAllocator,
    view: ShardView,
    journal: Journal,
    version: u64,
    connections: BTreeMap<ConnectionId, AttachedConnection>,
    /// The version each session first appeared at, threaded through so it
    /// survives recompilation.
    since: BTreeMap<SessionId, u64>,
    /// The inputs the routing table was last compiled from. Comparing against
    /// them is what makes a channel rename cost nothing on the audio plane.
    declared_audio: AudioRelation,
    session_of: BTreeMap<ConnectionId, SessionId>,
    routing: watch::Sender<Arc<AudioRouting>>,
}

impl<L: ShardLogic> Shard<L> {
    #[must_use]
    pub fn new(id: ShardId, logic: L) -> Shard<L> {
        let (routing, _) = watch::channel(Arc::new(AudioRouting::default()));
        Shard {
            id,
            logic,
            ids: IdAllocator::new(),
            view: ShardView::empty(),
            journal: Journal::new(),
            version: 0,
            connections: BTreeMap::new(),
            since: BTreeMap::new(),
            declared_audio: AudioRelation::default(),
            session_of: BTreeMap::new(),
            routing,
        }
    }

    #[must_use]
    pub fn id(&self) -> ShardId {
        self.id
    }

    #[must_use]
    pub fn version(&self) -> u64 {
        self.version
    }

    #[must_use]
    pub fn view(&self) -> &ShardView {
        &self.view
    }

    #[must_use]
    pub fn connection(&self, connection: ConnectionId) -> Option<&AttachedConnection> {
        self.connections.get(&connection)
    }

    /// The attached connections, in id order.
    pub fn connections(&self) -> impl Iterator<Item = &AttachedConnection> {
        self.connections.values()
    }

    /// The flavor's own state.
    ///
    /// The shard owns the logic outright, so this is how a composition binary
    /// or a test reaches the business model it handed over. A flavor driven by
    /// its own channel does not need it.
    pub fn logic_mut(&mut self) -> &mut L {
        &mut self.logic
    }

    /// A reader of the audio routing table, for the voice plane.
    #[must_use]
    pub fn routing(&self) -> watch::Receiver<Arc<AudioRouting>> {
        self.routing.subscribe()
    }

    /// Apply a command. Never renders: the caller reconciles afterwards.
    pub fn handle(&mut self, command: ShardCommand) {
        match command {
            ShardCommand::Attach { connection, queue } => self.attach(connection, queue),
            ShardCommand::Detach { connection, reason } => self.detach(connection, &reason),
            ShardCommand::Drained(connection) => self.retry(connection),
        }
    }

    /// Attach a connection.
    ///
    /// It starts observing **nothing**, which is what makes attaching an
    /// ordinary scope change rather than a mechanism of its own: the next
    /// reconcile sees its observation move away from empty, takes the slow path,
    /// and plans from the empty view. Attach, detach and migrate are then all
    /// the same code (guide 9.6).
    fn attach(&mut self, connection: ConnectionId, queue: OutboundQueue) {
        let Ok(session) = self.ids.session(Occupant::Connection(connection)) else {
            // The session space is exhausted. Refuse the connection rather than
            // attaching one that can never be rendered.
            queue.mark_fatal();
            return;
        };

        self.connections.insert(
            connection,
            AttachedConnection {
                id: connection,
                session,
                cursor: self.version,
                see: ScopeSet::NONE,
                overlay_sent: Overlay::default(),
                queue,
                shared_cursor: Arc::new(AtomicU64::new(self.version)),
            },
        );
        self.logic.observe(&VoiceEvent::Connected { connection });
    }

    /// Detach a connection: it stops hearing and being heard immediately, and is
    /// told to tear its view down.
    fn detach(&mut self, connection: ConnectionId, reason: &str) {
        let Some(mut attached) = self.connections.remove(&connection) else {
            return;
        };

        // The teardown plan is what makes a migration work: the connection's
        // socket survives, so it has to be told to forget this shard's subtree
        // before the next one starts describing its own.
        let before = self
            .view
            .restrict(attached.see)
            .compose(&attached.overlay_sent);
        let mut ops: Vec<PlanOp> = plan(&before, &ShardView::empty())
            .into_iter()
            .map(|planned| planned.op)
            .collect();
        collapse(&mut ops);
        if !ops.is_empty() {
            // A connection on its way out has nothing to retry with, so a
            // refusal here is an outcome rather than a fault.
            match attached.queue.try_send_all(emit(&ops, attached.session)) {
                Ok(()) => attached.commit(self.version, ScopeSet::NONE, Overlay::default()),
                Err(_refused) => attached.queue.mark_fatal(),
            }
        }

        self.logic.observe(&VoiceEvent::Disconnected {
            connection,
            reason: reason.to_owned(),
        });
    }

    /// Retry one connection whose queue drained, without touching the others.
    fn retry(&mut self, connection: ConnectionId) {
        let head = self.journal.head();
        if let Some(attached) = self.connections.get_mut(&connection) {
            // The overlay is whatever it last received: a retry replays the
            // shared journal, and any overlay change will arrive with the next
            // render. Cloning here keeps `push` free of a self-borrow.
            let overlay = attached.overlay_sent.clone();
            let _outcome = push(attached, &self.journal, head, &overlay);
        }
    }

    /// Render, plan, journal, publish routing, and push to every connection.
    pub fn reconcile(&mut self) -> ReconcileReport {
        let attached: Vec<ConnectionId> = self.connections.keys().copied().collect();
        let observations: BTreeMap<ConnectionId, ScopeSet> = attached
            .iter()
            .map(|connection| (*connection, self.logic.observation(*connection)))
            .collect();

        let rendered = {
            let mut builder = ShardBuilder::new(&mut self.ids, &attached);
            self.logic.render(&mut builder);
            match builder.finish(&observations) {
                Ok(rendered) => rendered,
                Err(refused) => {
                    // Rule 4: keep the current view, close nothing. The
                    // committed view is still correct and reconnecting would
                    // only reproduce the same broken render.
                    return ReconcileReport {
                        version: self.version,
                        refused: Some(refused),
                        ..ReconcileReport::default()
                    };
                }
            }
        };

        let ops = plan(&self.view, &rendered.view);
        let moved: Vec<ConnectionId> = attached
            .iter()
            .copied()
            .filter(|connection| {
                let observed = observations
                    .get(connection)
                    .copied()
                    .unwrap_or(ScopeSet::NONE);
                self.connections
                    .get(connection)
                    .is_some_and(|attached| attached.see != observed)
            })
            .collect();
        let overlays_changed = attached.iter().any(|connection| {
            let fresh = rendered.overlays.get(connection);
            let sent = self.connections.get(connection).map(|c| &c.overlay_sent);
            match (fresh, sent) {
                (Some(fresh), Some(sent)) => fresh != sent,
                (Some(fresh), None) => !fresh.is_empty(),
                (None, Some(sent)) => !sent.is_empty(),
                (None, None) => false,
            }
        });

        if ops.is_empty() && moved.is_empty() && !overlays_changed {
            return ReconcileReport {
                version: self.version,
                ..ReconcileReport::default()
            };
        }

        // The version indexes the journal, and only the shared delta goes in
        // it: an observation change is replanned from the views directly and an
        // overlay change is recomputed from `overlay_sent`. So an empty delta
        // must not advance the version, or a connection that stays congested
        // would churn versions every turn and push the journal's tail past what
        // its peers still need.
        let delta_len = ops.len();
        let published = !ops.is_empty();
        if published {
            self.version = self.version.saturating_add(1);
            self.journal.push(ops);
        }
        let previous = std::mem::replace(&mut self.view, rendered.view);

        // A session that has just appeared is dated to this version, so the
        // voice plane can tell a receiver has not been told about it yet.
        let mut since_changed = false;
        for session in self.view.users.keys() {
            if !self.since.contains_key(session) {
                self.since.insert(*session, self.version);
                since_changed = true;
            }
        }

        // Rule 2: publish routing BEFORE pushing views.
        let session_of: BTreeMap<ConnectionId, SessionId> = self
            .view
            .users
            .values()
            .filter_map(|user| user.occupant.connection().map(|c| (c, user.session)))
            .collect();
        let routing_recompiled =
            since_changed || rendered.audio != self.declared_audio || session_of != self.session_of;
        if routing_recompiled {
            let table = compile(&rendered.audio, &session_of, &self.since);
            // A receiver with no reader is not an error: the voice plane
            // subscribes when it starts, and a shard may outlive it.
            let _delivered = self.routing.send(Arc::new(table));
            self.declared_audio = rendered.audio;
            self.session_of = session_of;
        }

        let head = self.version;
        let moved_set: BTreeSet<ConnectionId> = moved.iter().copied().collect();
        let mut closed = Vec::new();
        for attached in self.connections.values_mut() {
            let overlay = rendered
                .overlays
                .get(&attached.id)
                .cloned()
                .unwrap_or_default();
            let outcome = if moved_set.contains(&attached.id) {
                let new_see = observations
                    .get(&attached.id)
                    .copied()
                    .unwrap_or(ScopeSet::NONE);
                replan(attached, &previous, &self.view, head, new_see, &overlay)
            } else {
                push(attached, &self.journal, head, &overlay)
            };
            if outcome == Outcome::Close {
                attached.queue.mark_fatal();
                closed.push(attached.id);
            }
        }

        ReconcileReport {
            version: self.version,
            published,
            delta_len,
            replanned: moved,
            routing_recompiled,
            closed,
            refused: None,
        }
    }
}

/// What a push or replan concluded for one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Delivered, or nothing to deliver.
    Advanced,
    /// The queue was full. Nothing moved; `Drained` will retry.
    Congested,
    /// Unrecoverable: tear the connection down and let it rebuild.
    Close,
}

/// Fast path: this connection's observation has not moved.
fn push(
    attached: &mut AttachedConnection,
    journal: &Journal,
    head: u64,
    overlay: &Overlay,
) -> Outcome {
    let Ok(replayed) = journal.replay(attached.cursor, head) else {
        // Below the tail: unrepairable from deltas, and dying anyway.
        return Outcome::Close;
    };

    let shared = filter(replayed, attached.see);
    let private = plan_elements(&attached.overlay_sent, overlay);
    let mut ops = crate::compose::splice(shared, private);
    collapse(&mut ops);

    if ops.is_empty() {
        // Nothing visible changed for it. Advance the cursor anyway, otherwise
        // a connection that sees nothing change would drift off the tail of the
        // journal and be closed for no reason.
        attached.commit(head, attached.see, overlay.clone());
        return Outcome::Advanced;
    }

    let session = attached.session;
    match attached.queue.try_send_all(emit(&ops, session)) {
        Ok(()) => {
            attached.commit(head, attached.see, overlay.clone());
            Outcome::Advanced
        }
        Err(Refused::Congested { .. }) => Outcome::Congested,
        Err(Refused::TooLarge { .. } | Refused::Closed) => Outcome::Close,
    }
}

/// Slow path: this connection's observation moved.
///
/// It has to learn its whole new subtree and forget the old one. No delta can
/// shorten that: the information it lacks is in **no** change, it was already
/// there.
///
/// This is the ordinary planner over two real views, so every ordering rule
/// holds by construction. It is deliberately not a detach followed by an
/// attach: whatever the two observations have in common - the public channels -
/// is left untouched, so the connection's own tree does not flicker.
fn replan(
    attached: &mut AttachedConnection,
    before: &ShardView,
    after: &ShardView,
    head: u64,
    new_see: ScopeSet,
    overlay: &Overlay,
) -> Outcome {
    let from = before
        .restrict(attached.see)
        .compose(&attached.overlay_sent);
    let to = after.restrict(new_see).compose(overlay);

    let mut ops: Vec<PlanOp> = plan(&from, &to)
        .into_iter()
        .map(|planned| planned.op)
        .collect();
    collapse(&mut ops);

    if ops.is_empty() {
        attached.commit(head, new_see, overlay.clone());
        return Outcome::Advanced;
    }

    let session = attached.session;
    match attached.queue.try_send_all(emit(&ops, session)) {
        // Rule 3: the triplet advances together, and only here. The guide's
        // sketch assigns the new observation before sending; doing that would
        // let a congested connection keep the new observation with the old
        // view, and the next fast-path filter would use a scope set the client
        // was never told about.
        Ok(()) => {
            attached.commit(head, new_see, overlay.clone());
            Outcome::Advanced
        }
        Err(Refused::Congested { .. }) => Outcome::Congested,
        Err(Refused::TooLarge { .. } | Refused::Closed) => Outcome::Close,
    }
}

/// The handle a flavor uses to say "my state changed".
///
/// Holds no state: a signal and a command channel. It is the only
/// business-to-runtime interface, and it carries no data.
#[derive(Debug, Clone)]
pub struct ShardHandle {
    shard: ShardId,
    wake: Arc<Notify>,
    commands: mpsc::Sender<ShardCommand>,
}

impl ShardHandle {
    #[must_use]
    pub fn shard(&self) -> ShardId {
        self.shard
    }

    /// "My state changed, re-render me when you can."
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Queue a command for the shard's task.
    ///
    /// # Errors
    ///
    /// The command itself, when the shard's task has ended.
    pub fn send(
        &self,
        command: ShardCommand,
    ) -> Result<(), mpsc::error::TrySendError<ShardCommand>> {
        self.commands.try_send(command)
    }
}

/// How many commands a shard's mailbox holds before senders are refused.
const MAILBOX_DEPTH: usize = 256;

/// Drive a shard until every handle to it is dropped.
///
/// Takes the driver halves produced by [`spawn_parts`] and gives the shard back,
/// so a caller that wants to inspect or migrate it can.
pub async fn run<L: ShardLogic>(
    mut shard: Shard<L>,
    wake: Arc<Notify>,
    mut mailbox: mpsc::Receiver<ShardCommand>,
) -> Shard<L> {
    loop {
        tokio::select! {
            // Cancellation-safe: `Notified` returns its permit on drop, so a
            // wake-up racing with a command is not lost, only deferred to the
            // next iteration.
            () = wake.notified() => {}
            // Cancellation-safe: `recv` does not consume a message unless it
            // completes.
            command = mailbox.recv() => match command {
                Some(command) => shard.handle(command),
                // Every handle is gone; nothing can ever wake this shard again.
                None => break,
            },
        }

        let _report = shard.reconcile();
        tokio::time::sleep(MIN_INTERVAL).await;
    }

    shard
}

/// The handle and the driver halves for one shard.
///
/// Split out so a test can drive [`Shard::handle`] and [`Shard::reconcile`] by
/// hand without a task at all, which is what keeps the whole control plane
/// testable without a clock.
#[must_use]
pub fn spawn_parts(shard: ShardId) -> (ShardHandle, Arc<Notify>, mpsc::Receiver<ShardCommand>) {
    let wake = Arc::new(Notify::new());
    let (commands, mailbox) = mpsc::channel(MAILBOX_DEPTH);
    (
        ShardHandle {
            shard,
            wake: Arc::clone(&wake),
            commands,
        },
        wake,
        mailbox,
    )
}
