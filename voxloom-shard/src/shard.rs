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
//! 3. The committed state of a connection is a **quadruplet** - cursor,
//!    observation, overlay, offered actions - and the four only advance together,
//!    once the queue has accepted everything. This is what lets a lagging
//!    connection catch up naturally: the shared part replays from the journal,
//!    the private parts are recomputed from what was last accepted, so neither an
//!    overlay nor an action ever needs journalling.
//! 4. An invalid render never replaces the current view: log it, keep the old
//!    one, and do not close anything.
//!
//! REF: docs/design/guide-implementation.md 9

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio::time::Instant;
use voxloom_protocol::ControlMessage;

use crate::build::{BuildError, ShardBuilder};
use crate::compose::{collapse, filter};
use crate::emit::{TextTarget, emit};
use crate::ids::{
    ActionKey, ChannelId, ChannelKey, ConnectionId, Occupant, SessionId, ShardId, SharedIds,
};
use crate::journal::Journal;
use crate::plan::{PlanOp, plan, plan_elements};
use crate::queue::{OutboundQueue, Refused};
use crate::reply::{Audience, Effects, Reply, Spoken};
use crate::routing::{AudioRelation, AudioRouting, Silence, compile};
use crate::scope::ScopeSet;
use crate::view::{Actions, Channel, On, Overlay, ShardView, User};

/// The floor between two publications.
///
/// Commands and flavor events are still absorbed immediately while this
/// cooldown runs. Only reconciliation is rate-limited, so a burst becomes one
/// publication instead of one 50 ms delay per command.
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
    /// A connection left for another shard. Its socket is still alive and its
    /// session is unchanged; only this shard stops describing it.
    ///
    /// Distinct from [`VoiceEvent::Disconnected`] because the two mean opposite
    /// things to a flavor: a disconnect frees a slot, a migration hands it over.
    Migrated {
        connection: ConnectionId,
        to: ShardId,
    },
    /// The client asked to enter a channel, by double-clicking it or dragging
    /// itself into it.
    ///
    /// A request, never a fact: the channel is one this connection can actually
    /// see, and nothing has moved. What it means is entirely the flavor's
    /// business, up to and including ignoring it.
    ///
    /// REF: references/vendored/Mumble.proto : `UserState.channel_id` sent by a
    ///   client for its own session.
    RequestedChannel {
        connection: ConnectionId,
        channel: ChannelKey,
    },
    /// The client asked to mute or deafen **itself**.
    ///
    /// A request like the others: the flags a client sees are the ones the
    /// flavor renders, so refusing is simply rendering nothing new. A flavor
    /// that grants it stores the pair and hands it back through
    /// [`crate::build::ShardBuilder::user_flags`].
    ///
    /// `None` means the client said nothing about that flag, so whatever the
    /// flavor currently renders stands. The runtime holds no copy of the pair:
    /// that state belongs to the flavor, and keeping a second one here is how
    /// the two start disagreeing.
    ///
    /// REF: references/mumble/src/mumble/ServerHandler.cpp :
    ///   `setSelfMuteDeafState` sends a `UserState` carrying both flags.
    RequestedSelfState {
        connection: ConnectionId,
        self_mute: Option<bool>,
        self_deaf: Option<bool>,
    },
    /// The client invoked a context action this shard had offered it.
    ///
    /// Everything is already resolved against what that connection was actually
    /// granted and can actually see: the key was offered to it, the target is
    /// visible to it, and the place matches the bits the flavor declared. What
    /// the action *means* is the flavor's business alone, including doing
    /// nothing and refusing through [`crate::reply::Reply::refuse`].
    ///
    /// REF: references/mumble/src/mumble/MainWindow.cpp : `context_triggered`
    ///   sends back the identifier the server stored, with the selected user
    ///   and channel.
    InvokedAction {
        connection: ConnectionId,
        action: ActionKey,
        on: ActionTarget,
    },
    /// The client typed something and aimed it somewhere.
    ///
    /// A request like every other: the target is already resolved against what
    /// this connection can see, and the channel it names is one the flavor
    /// rendered as writable, but **nothing has been delivered**. A flavor that
    /// ignores this event delivers nothing, which is the honest default for a
    /// runtime whose whole subject is who may know what.
    ///
    /// [`crate::reply::Reply::relay`] is the one line that carries it out, and
    /// the flavor is free to rewrite the audience, the text, or both.
    ///
    /// REF: references/mumble/src/murmur/Messages.cpp : `msgTextMessage` resolves
    ///   the targets against the sender's view, then routes.
    Said {
        connection: ConnectionId,
        to: Audience,
        text: String,
    },
}

/// What a context action was invoked on.
///
/// In the flavor's own vocabulary rather than the wire's: a key for a channel,
/// an occupant for a user, exactly like [`VoiceEvent::RequestedChannel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionTarget {
    /// Nothing was selected: the action was invoked on the server itself.
    Server,
    Channel(ChannelKey),
    User(Occupant),
}

/// What a flavor writes.
///
/// `Send + 'static` and deliberately **not** `Sync`: the runtime never needs to
/// share the logic, so it imposes no synchronization. A concrete flavor may
/// happen to be `Sync` if it holds one, which is its own business.
///
/// # External business input
///
/// [`ShardHandle::send`] is deliberately limited to [`ShardCommand`]: those are
/// runtime commands, not an extensible business mailbox. A concrete flavor owns
/// the transport for its own vocabulary instead. For an ordered event stream,
/// keep an `mpsc::Receiver<Event>` in the logic and give its senders to the
/// integration. For last-value-wins state, the same boundary can use a
/// `watch::Receiver` holding an immutable snapshot.
///
/// Slow or asynchronous work happens on the producer side. Once it has produced
/// an owned event or snapshot, the producer publishes it and then calls
/// [`ShardHandle::wake`]:
///
/// ```no_run
/// use tokio::sync::mpsc;
/// use voxloom_shard::{
///     ConnectionId, Reply, ScopeSet, ShardBuilder, ShardHandle, ShardLogic, VoiceEvent,
/// };
///
/// struct Notification;
/// struct GameEvent;
/// struct GameState;
///
/// impl GameState {
///     fn apply(&mut self, _event: GameEvent) {}
///
///     fn render(&self, _out: &mut ShardBuilder<'_>) {}
/// }
///
/// struct GameLogic {
///     inbox: mpsc::Receiver<GameEvent>,
///     state: GameState,
/// }
///
/// fn build_logic() -> (mpsc::Sender<GameEvent>, GameLogic) {
///     let (sender, inbox) = mpsc::channel(64);
///     (sender, GameLogic { inbox, state: GameState })
/// }
///
/// impl ShardLogic for GameLogic {
///     fn render(&mut self, out: &mut ShardBuilder<'_>) {
///         while let Ok(event) = self.inbox.try_recv() {
///             self.state.apply(event);
///         }
///         self.state.render(out);
///     }
///
///     fn observation(&mut self, _connection: ConnectionId) -> ScopeSet {
///         ScopeSet::NONE
///     }
///
///     fn observe(&mut self, _event: &VoiceEvent, _out: &mut Reply) {}
/// }
///
/// async fn calculate(_notification: Notification) -> GameEvent {
///     GameEvent
/// }
///
/// async fn publish(
///     notification: Notification,
///     sender: &mpsc::Sender<GameEvent>,
///     handle: &ShardHandle,
/// ) -> Result<(), mpsc::error::SendError<GameEvent>> {
///     let event = calculate(notification).await;
///     sender.send(event).await?;
///     handle.wake();
///     Ok(())
/// }
/// ```
///
/// `wake` carries no data. It only says that the desired state may have
/// changed, so several wake-ups may be coalesced into one reconciliation. The
/// channel or snapshot remains the source of truth. Publishing before waking
/// ensures that the next [`ShardLogic::render`] can observe the change.
///
/// Keeping `render` synchronous is intentional: it must not wait for I/O or
/// perform blocking work. Its `&mut self` receiver lets it drain the
/// flavor-owned channel and update local state without a lock. Any `.await`
/// belongs before publication, outside the shard task, as in the example.
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
    ///
    /// `out` is the only way a flavor speaks: everything else it wants to change
    /// belongs to the next [`ShardLogic::render`]. See [`Reply`] for why the two
    /// doors are separate.
    fn observe(&mut self, event: &VoiceEvent, out: &mut Reply);
}

/// A command for a shard's task.
#[derive(Debug)]
pub enum ShardCommand {
    Attach {
        connection: ConnectionId,
        /// Shared with the connection's writer task, which is what lets a
        /// migration hand the same queue to the next shard.
        queue: Arc<OutboundQueue>,
        /// The cell the voice plane gates on. Owned by the runtime rather than
        /// by the shard, so a migration does not have to republish it.
        cursor: Arc<AtomicU64>,
        /// What the client already holds. Empty for a fresh connection, and the
        /// previous shard's composed view for a migration.
        held: ShardView,
        /// Fired once the connection's first transition has been accepted, so
        /// the handshake knows when it may send `ServerSync`.
        ready: Option<oneshot::Sender<()>>,
    },
    Detach {
        connection: ConnectionId,
        reason: String,
        /// Set when the connection is moving to another shard rather than
        /// leaving. It receives the view the client still holds, and the
        /// teardown is **skipped**: the destination plans one transition from
        /// that view instead.
        handover: Option<Handover>,
    },
    /// A connection's queue drained: retry for **that one alone**, in O(1).
    Drained(ConnectionId),
    /// The client asked to enter a channel. Resolved against this connection's
    /// own view and reported to the flavor, or refused.
    Requested {
        connection: ConnectionId,
        channel: ChannelId,
    },
    /// The client asked to mute or deafen itself. Reported to the flavor, which
    /// alone decides what the view ends up saying.
    RequestedSelfState {
        connection: ConnectionId,
        self_mute: Option<bool>,
        self_deaf: Option<bool>,
    },
    /// The client asked what it may do in a channel. Answered from the render,
    /// for that connection alone.
    QueriedPermissions {
        connection: ConnectionId,
        channel: ChannelId,
    },
    /// The client asked what the server publishes about a user. Answered only
    /// for a user this connection can actually see.
    QueriedUserStats {
        connection: ConnectionId,
        target: SessionId,
    },
    /// The client invoked a context action. Validated against what this
    /// connection was offered and what it can see, then reported to the flavor.
    ///
    /// The wire identifier travels as it arrived: the gateway does not know
    /// which actions exist, and reading it back is part of the validation.
    InvokedAction {
        connection: ConnectionId,
        action: String,
        session: Option<SessionId>,
        channel: Option<ChannelId>,
    },
    /// The client sent a text message. Resolved against this connection's own
    /// view and reported to the flavor, or refused.
    ///
    /// The gateway has already checked the shape, the length and the rate; what
    /// is left is the only question it cannot answer, which is whether this
    /// connection may name that target at all.
    Said {
        connection: ConnectionId,
        to: TextTarget,
        message: String,
    },
}

impl ShardCommand {
    /// Attach a connection that holds nothing yet: a fresh arrival.
    #[must_use]
    pub fn attach(connection: ConnectionId, queue: Arc<OutboundQueue>) -> ShardCommand {
        ShardCommand::Attach {
            connection,
            queue,
            cursor: Arc::new(AtomicU64::new(0)),
            held: ShardView::empty(),
            ready: None,
        }
    }

    /// Detach a connection that is leaving for good.
    #[must_use]
    pub fn detach(connection: ConnectionId, reason: impl Into<String>) -> ShardCommand {
        ShardCommand::Detach {
            connection,
            reason: reason.into(),
            handover: None,
        }
    }
}

/// Where a migrating connection's held view is sent.
#[derive(Debug)]
pub struct Handover {
    /// The shard the connection is moving to, reported to the flavor.
    pub to: ShardId,
    /// Receives the composed view the client still holds.
    pub view: oneshot::Sender<ShardView>,
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
    /// What context actions it has been offered. Same discipline as the overlay:
    /// what the client **holds**, not what the last render wanted it to hold.
    actions_sent: Actions,
    /// What the client holds, when this shard cannot derive it from its own
    /// previous view: right after an attach, and right after a migration.
    ///
    /// `Some` only until the first transition is accepted, and then never again.
    /// Keeping one view per connection permanently is exactly the O(N·W) memory
    /// this whole design exists to avoid, so the field is a transient rather
    /// than a cache.
    held: Option<ShardView>,
    /// Fired once, when the connection's first transition lands.
    ready: Option<oneshot::Sender<()>>,
    queue: Arc<OutboundQueue>,
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

    /// The context actions this connection has actually been offered.
    #[must_use]
    pub fn actions_sent(&self) -> &Actions {
        &self.actions_sent
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

    /// Advance the committed quadruplet. The only place the four move, and they
    /// move together.
    fn commit(&mut self, cursor: u64, see: ScopeSet, overlay: Overlay, actions: Actions) {
        self.cursor = cursor;
        self.see = see;
        self.overlay_sent = overlay;
        self.actions_sent = actions;
        self.shared_cursor.store(cursor, Ordering::Relaxed);
        // Whatever the client held before this transition is now described by
        // the triplet, so the transient copy is dropped rather than kept.
        self.held = None;
        if let Some(ready) = self.ready.take() {
            // Nobody waiting is the normal case for every connection but the
            // one still in its handshake.
            let _awaited = ready.send(());
        }
    }

    /// The view this connection's client actually holds right now.
    fn composed(&self, shared: &ShardView) -> ShardView {
        match &self.held {
            Some(held) => held.clone(),
            None => shared.restrict(self.see).compose(&self.overlay_sent),
        }
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
    ids: SharedIds,
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
    /// Part of the same comparison: who was muted or deafened last time.
    declared_silence: Silence,
    session_of: BTreeMap<ConnectionId, SessionId>,
    routing: watch::Sender<Arc<AudioRouting>>,
    /// Where what this shard cannot do itself is sent. `None` for a shard that
    /// belongs to no runtime, which is a real state - a test, a benchmark - and
    /// not a missing wire, so it is reported rather than assumed away.
    effects: Option<Effects>,
}

impl<L: ShardLogic> Shard<L> {
    /// A shard with an allocator of its own.
    ///
    /// Correct for a runtime that will only ever hold one shard. As soon as a
    /// connection can move, every shard must share one allocator: see
    /// [`Shard::with_ids`] and [`SharedIds`].
    #[must_use]
    pub fn new(id: ShardId, logic: L) -> Shard<L> {
        Shard::with_ids(id, logic, SharedIds::new())
    }

    /// A shard drawing its identifiers from a runtime-wide allocator.
    #[must_use]
    pub fn with_ids(id: ShardId, logic: L, ids: SharedIds) -> Shard<L> {
        let (routing, _) = watch::channel(Arc::new(AudioRouting::default()));
        Shard {
            id,
            logic,
            ids,
            view: ShardView::empty(),
            journal: Journal::new(),
            version: 0,
            connections: BTreeMap::new(),
            since: BTreeMap::new(),
            declared_audio: AudioRelation::default(),
            declared_silence: Silence::default(),
            session_of: BTreeMap::new(),
            routing,
            effects: None,
        }
    }

    /// Wire this shard's effects to a runtime.
    ///
    /// Kept out of the constructors on purpose: a shard is complete without one,
    /// and only the code that owns several shards can honour a move between two
    /// of them.
    pub fn route_effects(&mut self, effects: Effects) {
        self.effects = Some(effects);
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
            ShardCommand::Attach {
                connection,
                queue,
                cursor,
                held,
                ready,
            } => self.attach(connection, queue, cursor, held, ready),
            ShardCommand::Detach {
                connection,
                reason,
                handover,
            } => self.detach(connection, &reason, handover),
            ShardCommand::Drained(connection) => self.retry(connection),
            ShardCommand::Requested {
                connection,
                channel,
            } => self.requested(connection, channel),
            ShardCommand::RequestedSelfState {
                connection,
                self_mute,
                self_deaf,
            } => self.requested_self_state(connection, self_mute, self_deaf),
            ShardCommand::QueriedPermissions {
                connection,
                channel,
            } => self.queried_permissions(connection, channel),
            ShardCommand::QueriedUserStats { connection, target } => {
                self.queried_user_stats(connection, target);
            }
            ShardCommand::InvokedAction {
                connection,
                action,
                session,
                channel,
            } => self.invoked(connection, &action, session, channel),
            ShardCommand::Said {
                connection,
                to,
                message,
            } => self.said(connection, to, message),
        }
    }

    /// Attach a connection.
    ///
    /// It starts observing **nothing** and holding `held`, which is what makes
    /// attaching an ordinary scope change rather than a mechanism of its own:
    /// the next reconcile sees a connection that holds a view this shard did not
    /// produce, takes the slow path, and plans one transition from it. Attach,
    /// detach and migrate are then all the same code (guide 9.6), and a
    /// migration inherits the no-flicker property for free.
    fn attach(
        &mut self,
        connection: ConnectionId,
        queue: Arc<OutboundQueue>,
        cursor: Arc<AtomicU64>,
        held: ShardView,
        ready: Option<oneshot::Sender<()>>,
    ) {
        let Ok(session) = self.ids.session(Occupant::Connection(connection)) else {
            // The session space is exhausted. Refuse the connection rather than
            // attaching one that can never be rendered.
            queue.mark_fatal();
            return;
        };

        cursor.store(self.version, Ordering::Relaxed);
        self.connections.insert(
            connection,
            AttachedConnection {
                id: connection,
                session,
                cursor: self.version,
                see: ScopeSet::NONE,
                overlay_sent: Overlay::default(),
                actions_sent: Actions::default(),
                held: Some(held),
                ready,
                queue,
                shared_cursor: cursor,
            },
        );
        self.tell(&VoiceEvent::Connected { connection });
    }

    /// Detach a connection: it stops hearing and being heard immediately.
    ///
    /// A connection that is **leaving** is told to tear its view down. One that
    /// is **migrating** is not: its held view is handed to the destination,
    /// which plans a single transition onto its own tree. Tearing down first
    /// would be worse than wasteful, it would be wrong - the client keeps itself
    /// in its own model after a `UserRemove` for its own session, so the
    /// following `ChannelRemove` would look like a removal of an occupied
    /// channel and the client would disconnect over a protocol violation.
    ///
    /// REF: references/mumble/src/mumble/Messages.cpp : `msgUserRemove` skips
    ///   `removeUser` when the victim is self; `msgChannelRemove` disconnects
    ///   when `UserModel::removeChannel(c, true)` refuses an occupied channel.
    fn detach(&mut self, connection: ConnectionId, reason: &str, handover: Option<Handover>) {
        let Some(mut attached) = self.connections.remove(&connection) else {
            return;
        };

        let held = attached.composed(&self.view);
        if let Some(handover) = handover {
            // An action belongs to the shard that offered it, and the destination
            // starts from an empty registry because that is what an attach says.
            // Without this the client would keep buttons nobody can ever withdraw,
            // and invoking one would reach a shard that never granted it.
            let withdrawn = crate::emit::actions(&attached.actions_sent, &Actions::default());
            if !withdrawn.is_empty()
                && let Err(refused) = attached.queue.try_send_all(withdrawn)
            {
                eprintln!(
                    "voxloom-shard: shard {:?}: {connection:?} leaves with context actions this \
                     shard could not withdraw: {refused}",
                    self.id
                );
            }
            // A closed receiver means the migration was abandoned between the
            // two commands; the connection is then attached nowhere, and its
            // own task tears it down.
            let _received = handover.view.send(held);
            self.tell(&VoiceEvent::Migrated {
                connection,
                to: handover.to,
            });
            return;
        }

        let mut ops: Vec<PlanOp> = plan(&held, &ShardView::empty())
            .into_iter()
            .map(|planned| planned.op)
            .collect();
        collapse(&mut ops);
        if !ops.is_empty() {
            // A connection on its way out has nothing to retry with, so a
            // refusal here is an outcome rather than a fault.
            match attached.queue.try_send_all(emit(&ops, attached.session)) {
                Ok(()) => attached.commit(
                    self.version,
                    ScopeSet::NONE,
                    Overlay::default(),
                    Actions::default(),
                ),
                Err(_refused) => attached.queue.mark_fatal(),
            }
        }

        self.tell(&VoiceEvent::Disconnected {
            connection,
            reason: reason.to_owned(),
        });
    }

    /// Resolve a client's channel request against what that client can see.
    ///
    /// Refusing an id the connection does not observe is not politeness: it
    /// keeps a guessed number from working as an existence oracle for channels
    /// in another team's subtree.
    fn requested(&mut self, connection: ConnectionId, channel: ChannelId) {
        let Some(attached) = self.connections.get(&connection) else {
            return;
        };
        let key = self
            .visible_channel(attached, channel)
            .map(|rendered| rendered.key);

        match key {
            Some(key) => self.tell(&VoiceEvent::RequestedChannel {
                connection,
                channel: key,
            }),
            // Fail closed and stay audible: an operator seeing this repeatedly
            // is looking at either a stale client or a probe.
            None => eprintln!(
                "voxloom-shard: shard {:?}: {connection:?} asked for channel {channel:?}, which it \
                 cannot see",
                self.id
            ),
        }
    }

    /// A channel as this connection can see it: shared and observed, or held
    /// from before its first transition, or private to it.
    ///
    /// The three places are the whole of what a client may legitimately name.
    /// Answering about anything else, however harmlessly, would turn a guessed
    /// identifier into an existence oracle for another team's subtree.
    fn visible_channel<'a>(
        &'a self,
        attached: &'a AttachedConnection,
        channel: ChannelId,
    ) -> Option<&'a Channel> {
        self.view
            .channels
            .get(&channel)
            .filter(|rendered| attached.see.sees(rendered.scope))
            .or_else(|| {
                attached
                    .held
                    .as_ref()
                    .and_then(|held| held.channels.get(&channel))
            })
            .or_else(|| attached.overlay_sent.channels.get(&channel))
    }

    /// A user as this connection can see it. The same three places, and the same
    /// reason.
    fn seen_user<'a>(
        &'a self,
        attached: &'a AttachedConnection,
        session: SessionId,
    ) -> Option<&'a User> {
        self.view
            .users
            .get(&session)
            .filter(|user| attached.see.sees(user.scope))
            .or_else(|| {
                attached
                    .held
                    .as_ref()
                    .and_then(|held| held.users.get(&session))
            })
            .or_else(|| attached.overlay_sent.users.get(&session))
    }

    /// Resolve an invocation against what this connection was granted and what
    /// it can see, then report it.
    ///
    /// Three refusals, all silent to the client and audible to an operator, for
    /// the same reason [`Shard::requested`] refuses: an answer that varied with
    /// whether the target exists would turn a guessed identifier into an oracle.
    ///
    /// The target is chosen by the bits the flavor declared, most specific
    /// first, rather than by what the message carries. The client reads the
    /// tree's current selection whichever menu the action came from, so a server
    /// action routinely arrives with a session and a channel it has nothing to
    /// do with.
    ///
    /// REF: references/mumble/src/mumble/MainWindow.cpp : `context_triggered`
    ///   fills `session` and `channel_id` from `qtvUsers->currentIndex()`, while
    ///   server actions live in `qmServer` and are never told about it.
    fn invoked(
        &mut self,
        connection: ConnectionId,
        action: &str,
        session: Option<SessionId>,
        channel: Option<ChannelId>,
    ) {
        let Some(attached) = self.connections.get(&connection) else {
            return;
        };

        let Some(key) = crate::emit::action_key(action) else {
            eprintln!(
                "voxloom-shard: shard {:?}: {connection:?} invoked {action:?}, which is not a name \
                 this server writes",
                self.id
            );
            return;
        };
        // What the client holds, not what the last render wanted it to hold: an
        // action withdrawn in a turn this connection has not received yet is
        // still legitimately on its screen. The flavor keeps the last word and
        // can refuse out loud.
        let Some(offered) = attached.actions_sent.get(&key) else {
            eprintln!(
                "voxloom-shard: shard {:?}: {connection:?} invoked action {key:?}, which it was \
                 never offered",
                self.id
            );
            return;
        };
        let on = offered.on;

        let target = if let Some(session) = session.filter(|_| on.covers(On::USER)) {
            // Named a user and the action is about users: it must be a user this
            // connection has been told about, and no fallback softens that.
            self.seen_user(attached, session)
                .map(|user| ActionTarget::User(user.occupant))
        } else if let Some(channel) = channel.filter(|_| on.covers(On::CHANNEL)) {
            self.visible_channel(attached, channel)
                .map(|rendered| ActionTarget::Channel(rendered.key))
        } else if on.covers(On::SERVER) {
            Some(ActionTarget::Server)
        } else {
            None
        };

        match target {
            Some(on) => self.tell(&VoiceEvent::InvokedAction {
                connection,
                action: key,
                on,
            }),
            None => eprintln!(
                "voxloom-shard: shard {:?}: {connection:?} invoked action {key:?} on a target it \
                 cannot see, or that the action was not offered on",
                self.id
            ),
        }
    }

    /// Resolve where a client aimed a text message, then report it.
    ///
    /// Two different refusals, and the difference is the whole point:
    ///
    /// - A target this connection cannot see is **silent** to the client and
    ///   audible to an operator, like every other unseen target here. An answer
    ///   that varied with whether the id exists would turn a guessed number into
    ///   an existence oracle.
    /// - A target it *can* see but that the flavor rendered read-only is refused
    ///   **out loud**, with the missing permission. Nothing leaks: the client is
    ///   already holding that channel, and it asked to do something the answer to
    ///   `PermissionQuery` had already denied.
    ///
    /// A private message is checked against the recipient's channel rather than
    /// the sender's, which is what the reference server does.
    ///
    /// REF: references/mumble/src/murmur/Messages.cpp : `msgTextMessage` checks
    ///   `ChanACL::TextMessage` on each named channel, and on `u->cChannel` for a
    ///   directly addressed user.
    fn said(&mut self, connection: ConnectionId, to: TextTarget, message: String) {
        let Some(attached) = self.connections.get(&connection) else {
            return;
        };
        let session = attached.session;

        // Resolved into the flavor's vocabulary, and only through what this
        // connection actually holds. `writable_in` is the channel whose
        // `can_text` decides: the one named, or the recipient's own for a
        // private message.
        let resolved = match to {
            TextTarget::Session(target) => self
                .seen_user(attached, target)
                .map(|user| (Audience::User(user.occupant), user.channel)),
            TextTarget::Channel(channel) => self
                .visible_channel(attached, channel)
                .map(|rendered| (Audience::Channel(rendered.key), channel)),
            TextTarget::Tree(channel) => self
                .visible_channel(attached, channel)
                .map(|rendered| (Audience::Tree(rendered.key), channel)),
        };
        let Some((audience, writable_in)) = resolved else {
            eprintln!(
                "voxloom-shard: shard {:?}: {connection:?} wrote to {to:?}, which it cannot see",
                self.id
            );
            return;
        };

        // For a tree that is its root, deliberately: the flavor chooses the
        // audience it actually delivers to, so refusing further down would be
        // refusing on behalf of a decision it has not made yet.
        let writable = self
            .visible_channel(attached, writable_in)
            .is_some_and(|rendered| rendered.can_text);
        if !writable {
            self.answer(
                attached,
                crate::emit::denied_permission(
                    session,
                    writable_in,
                    crate::emit::perm::TEXT_MESSAGE,
                ),
            );
            return;
        }

        self.tell(&VoiceEvent::Said {
            connection,
            to: audience,
            text: message,
        });
    }

    /// Answer "what may I do in that channel", from the current render.
    ///
    /// The flavor is not consulted and has nothing to decide: it already said
    /// everything it had to say by rendering the channel, and a query is not an
    /// intent. Keeping it here also keeps it cheap - one lookup, one message -
    /// where a round trip through business code would cost a turn.
    fn queried_permissions(&self, connection: ConnectionId, channel: ChannelId) {
        let Some(attached) = self.connections.get(&connection) else {
            return;
        };
        match self.visible_channel(attached, channel) {
            Some(rendered) => self.answer(attached, crate::emit::permission_query(rendered)),
            None => eprintln!(
                "voxloom-shard: shard {:?}: {connection:?} queried permissions on channel \
                 {channel:?}, which it cannot see",
                self.id
            ),
        }
    }

    /// Answer "what do you publish about that user", for a user it can see.
    fn queried_user_stats(&self, connection: ConnectionId, target: SessionId) {
        let Some(attached) = self.connections.get(&connection) else {
            return;
        };
        if self.seen_user(attached, target).is_some() {
            self.answer(attached, crate::emit::user_stats(target));
        } else {
            eprintln!(
                "voxloom-shard: shard {:?}: {connection:?} queried stats about session {target:?}, \
                 which it cannot see",
                self.id
            );
        }
    }

    /// Push one message that answers a question, rather than describing a
    /// change.
    ///
    /// Refusing it is an outcome, not a fault: a query the client can simply ask
    /// again is worth less than the transitions queued ahead of it, so a
    /// congested connection drops the answer instead of being torn down for it.
    fn answer(&self, attached: &AttachedConnection, message: ControlMessage) {
        if let Err(refused) = attached.queue.try_send_all(vec![message]) {
            eprintln!(
                "voxloom-shard: shard {:?}: dropping an answer for {:?}: {refused}",
                self.id, attached.id
            );
        }
    }

    /// Report an event to the flavor, and deliver whatever it said.
    ///
    /// The single door between the runtime and the business model: the flavor
    /// takes the event, updates its own state, and writes into a [`Reply`] that
    /// borrows nothing, so what it says is delivered here rather than from
    /// inside its own call.
    ///
    /// A word aimed at a connection this shard does not hold is dropped with a
    /// log rather than refused loudly. That is the normal shape of a departure:
    /// [`Shard::detach`] removes the connection **before** reporting it, so a
    /// flavor saying goodbye is speaking to a socket that is already leaving.
    fn tell(&mut self, event: &VoiceEvent) {
        let mut reply = Reply::default();
        self.logic.observe(event, &mut reply);

        // Words first: a flavor that says goodbye and switches in the same
        // breath must have the farewell on the socket before the connection is
        // handed over.
        for (connection, words) in reply.drain() {
            let Some(attached) = self.connections.get(&connection) else {
                eprintln!(
                    "voxloom-shard: shard {:?}: dropping {} word(s) for {connection:?}, which is \
                     not attached here",
                    self.id,
                    words.len()
                );
                continue;
            };
            let messages: Vec<ControlMessage> = words
                .iter()
                .map(|word| crate::emit::spoken(attached.session, word))
                .collect();
            // Same bargain as an answer: speech the client can live without is
            // worth less than the transitions queued ahead of it.
            if let Err(refused) = attached.queue.try_send_all(messages) {
                eprintln!(
                    "voxloom-shard: shard {:?}: dropping what the flavor said to {connection:?}: \
                     {refused}",
                    self.id
                );
            }
        }

        for spoken in reply.drain_spoken() {
            self.deliver(&spoken);
        }

        for effect in reply.drain_effects() {
            match &self.effects {
                Some(route) => route(effect),
                None => eprintln!(
                    "voxloom-shard: shard {:?}: dropping {effect:?}: this shard belongs to no \
                     runtime, so nothing can carry it out",
                    self.id
                ),
            }
        }
    }

    /// Expand an audience and carry one message to each of its members.
    ///
    /// The expansion happens here rather than in the flavor because it is the
    /// only place that holds the view - and it is the composed view, per
    /// recipient, so an overlay placement counts as being somewhere just as much
    /// as a shared one does.
    ///
    /// Two members are left out, for reasons that are not politeness:
    ///
    /// - The speaker, which the reference server drops too. A client already
    ///   printed what it typed.
    /// - Anyone who cannot see the speaker. That is the audio coupling rule -
    ///   a receiver must see the sender - and it is also forced: a `TextMessage`
    ///   naming a session the recipient does not hold breaks the view invariants
    ///   the conformance model checks. A flavor that wants everyone to read
    ///   something whoever said it uses [`crate::reply::Reply::announce`].
    ///
    /// REF: references/mumble/src/murmur/Messages.cpp : `msgTextMessage` ends on
    ///   `users.remove(uSource)` before forwarding.
    fn deliver(&self, spoken: &Spoken) {
        let speaker = spoken
            .from
            .and_then(|connection| self.connections.get(&connection))
            .map(|attached| attached.session);
        if spoken.from.is_some() && speaker.is_none() {
            eprintln!(
                "voxloom-shard: shard {:?}: dropping a relay from {:?}, which is not attached here",
                self.id, spoken.from
            );
            return;
        }

        // Resolved once rather than per recipient: a key stands for the same id
        // whoever is looking, and the alternative is a scan of the render for
        // every connection in the shard.
        let Some(target) = self.resolve(spoken.to) else {
            eprintln!(
                "voxloom-shard: shard {:?}: dropping a message for {:?}, which names nothing this \
                 shard has rendered",
                self.id, spoken.to
            );
            return;
        };

        for attached in self.connections.values() {
            if Some(attached.id) == spoken.from {
                continue;
            }
            if !self.addressed(attached, target) {
                continue;
            }
            if let Some(session) = speaker
                && self.seen_user(attached, session).is_none()
            {
                eprintln!(
                    "voxloom-shard: shard {:?}: {:?} is in the audience but cannot see session \
                     {session:?}, so it is skipped rather than told the message came from nobody",
                    self.id, attached.id
                );
                continue;
            }
            // Same bargain as an answer: a message the client can live without
            // is worth less than the transitions queued ahead of it.
            if let Err(refused) = attached.queue.try_send_all(vec![crate::emit::relayed(
                speaker,
                target,
                &spoken.text,
            )]) {
                eprintln!(
                    "voxloom-shard: shard {:?}: dropping a relay to {:?}: {refused}",
                    self.id, attached.id
                );
            }
        }
    }

    /// Turn what a flavor named into what a client holds.
    ///
    /// The same identifier for every recipient, which is why this happens once
    /// per message: the wire ids are the shard's, not the observer's. What
    /// differs per observer is only whether they are *in* the audience, and that
    /// is [`Shard::addressed`].
    fn resolve(&self, audience: Audience) -> Option<TextTarget> {
        match audience {
            Audience::User(occupant) => self
                .ids
                .allocated_session(occupant)
                .map(TextTarget::Session),
            Audience::Channel(key) => self.channel_of(key).map(TextTarget::Channel),
            Audience::Tree(key) => self.channel_of(key).map(TextTarget::Tree),
        }
    }

    /// Whether `attached` is one of the recipients `target` stands for.
    ///
    /// Read through [`Shard::seen_user`], so a connection placed by an overlay is
    /// where its own client believes it is, which is the only answer that makes
    /// sense to the person reading the message.
    fn addressed(&self, attached: &AttachedConnection, target: TextTarget) -> bool {
        match target {
            TextTarget::Session(session) => session == attached.session,
            TextTarget::Channel(channel) => self
                .seen_user(attached, attached.session)
                .is_some_and(|user| user.channel == channel),
            TextTarget::Tree(root) => self
                .seen_user(attached, attached.session)
                .is_some_and(|user| self.descends_from(attached, user.channel, root)),
        }
    }

    /// The id a flavor's channel key currently stands for.
    ///
    /// The render is consulted first because the root does not go through the
    /// allocator at all - it *is* [`ChannelId::ROOT`] in every shard - and asking
    /// the allocator for it would answer "no such channel" about the one channel
    /// everybody can see. The allocator answers for the rest, including a channel
    /// that only lives in somebody's overlay.
    fn channel_of(&self, key: ChannelKey) -> Option<ChannelId> {
        self.view
            .channels
            .values()
            .find(|channel| channel.key == key)
            .map(|channel| channel.id)
            .or_else(|| self.ids.allocated_channel(self.id, key))
    }

    /// Whether `channel` is `root` or sits below it, walking the view this
    /// connection composes.
    ///
    /// Bounded by the tree it walks rather than by a counter: the root is its own
    /// parent, so the walk always ends there, and a channel whose parent has
    /// dropped out of view ends it early.
    fn descends_from(
        &self,
        attached: &AttachedConnection,
        channel: ChannelId,
        root: ChannelId,
    ) -> bool {
        let mut current = channel;
        loop {
            if current == root {
                return true;
            }
            let Some(rendered) = self.visible_channel(attached, current) else {
                return false;
            };
            if rendered.parent == current {
                return false;
            }
            current = rendered.parent;
        }
    }

    /// Report a client's request about its own audio state to the flavor.
    ///
    /// There is nothing to resolve against the view: the request names no
    /// element, only the connection making it, so the visibility question that
    /// guards [`Shard::requested`] does not arise. It is still refused for a
    /// connection this shard does not hold, so a command that raced a detach
    /// cannot reach the flavor as if the connection were still here.
    fn requested_self_state(
        &mut self,
        connection: ConnectionId,
        self_mute: Option<bool>,
        self_deaf: Option<bool>,
    ) {
        if !self.connections.contains_key(&connection) {
            eprintln!(
                "voxloom-shard: shard {:?}: {connection:?} asked to change its own state, but it \
                 is not attached here",
                self.id
            );
            return;
        }
        self.tell(&VoiceEvent::RequestedSelfState {
            connection,
            self_mute,
            self_deaf,
        });
    }

    /// Retry one connection whose queue drained, without touching the others.
    ///
    /// A retry is the **fast path and nothing else**: it replays the shared
    /// journal from the connection's cursor. A connection that still holds a view
    /// this shard did not produce - a fresh attach, or one just handed over by a
    /// migration - has no cursor into this journal that means anything, and the
    /// transition it is owed is a replan against the render. So it is left alone
    /// here, and [`Shard::reconcile`] does it: `held` keeps it in `moved` every
    /// turn until the transition actually lands.
    ///
    /// Pushing it anyway is not merely early, it is destructive: the replay from
    /// `cursor` to `head` is empty, `push` concludes there is nothing to send and
    /// commits, and committing drops `held`. The view the client really holds is
    /// then gone, the replan plans from an empty one, and every element of the
    /// source shard survives the migration with nothing left to remove it.
    fn retry(&mut self, connection: ConnectionId) {
        let head = self.journal.head();
        let mut retired_channels = BTreeSet::new();
        if let Some(attached) = self.connections.get_mut(&connection) {
            if attached.held.is_some() {
                return;
            }
            // The private parts are whatever it last received: a retry replays the
            // shared journal, and any private change will arrive with the next
            // render. Cloning here keeps `push` free of a self-borrow.
            let overlay = attached.overlay_sent.clone();
            let actions = attached.actions_sent.clone();
            let (_outcome, retired) = push(attached, &self.journal, head, &overlay, &actions);
            retired_channels = retired;
        }
        self.ids.retire_channel_ids(self.id, &retired_channels);
    }

    /// Render, plan, journal, publish routing, and push to every connection.
    pub fn reconcile(&mut self) -> ReconcileReport {
        let attached: Vec<ConnectionId> = self.connections.keys().copied().collect();
        let observations: BTreeMap<ConnectionId, ScopeSet> = attached
            .iter()
            .map(|connection| (*connection, self.logic.observation(*connection)))
            .collect();

        let rendered = {
            let mut builder = ShardBuilder::new(&self.ids, self.id, &attached);
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
                self.connections.get(connection).is_some_and(|attached| {
                    // A connection holding a view this shard did not produce
                    // has to be replanned whatever its observation says, and
                    // that includes the case where both are empty.
                    attached.held.is_some() || attached.see != observed
                })
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
        // Offering or withdrawing a button changes neither the shared view nor
        // any observation, so without this term the render would be believed
        // unchanged and the menu would never move.
        let actions_changed = attached.iter().any(|connection| {
            let fresh = rendered.actions.get(connection);
            let sent = self.connections.get(connection).map(|c| &c.actions_sent);
            match (fresh, sent) {
                (Some(fresh), Some(sent)) => fresh != sent,
                (Some(fresh), None) => !fresh.is_empty(),
                (None, Some(sent)) => !sent.is_empty(),
                (None, None) => false,
            }
        });

        if ops.is_empty() && moved.is_empty() && !overlays_changed && !actions_changed {
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
        let retained_channels: BTreeSet<ChannelKey> = rendered
            .view
            .channels
            .values()
            .chain(
                rendered
                    .overlays
                    .values()
                    .flat_map(|overlay| overlay.channels.values()),
            )
            .filter(|channel| channel.id != ChannelId::ROOT)
            .map(|channel| channel.key)
            .collect();
        let previous = std::mem::replace(&mut self.view, rendered.view);
        // The render is valid and has replaced the desired view. Any key absent
        // from both its shared and private parts has now been withdrawn from
        // every client view; if it returns, the wire id must not.
        self.ids.retain_channels(self.id, &retained_channels);

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
        //
        // The flags are read exactly where the sessions are, and from the same
        // presence: whichever rendering of a user is the one the audio plane
        // will name it by is also the one that decides whether it may speak.
        let mut session_of: BTreeMap<ConnectionId, SessionId> = BTreeMap::new();
        let mut silence = Silence::default();
        for user in self.view.users.values() {
            silence.record(user.session, user.flags);
            if let Some(connection) = user.occupant.connection() {
                session_of.insert(connection, user.session);
            }
        }
        // A connection with no shared presence is not absent from the runtime,
        // only from the shared view: that is exactly what a vanish is. Leaving it
        // out here would quietly turn "hears everything, heard by nobody" into
        // "takes no part in audio at all", and the flavor would have no way to
        // tell the two apart. Whether any given receiver may actually hear it is
        // a separate question, and the render already refuses a relation where
        // the answer is no.
        for (connection, overlay) in &rendered.overlays {
            if session_of.contains_key(connection) {
                continue;
            }
            let own = overlay
                .users
                .values()
                .find(|user| user.occupant == Occupant::Connection(*connection));
            if let Some(user) = own {
                session_of.insert(*connection, user.session);
                silence.record(user.session, user.flags);
            }
        }
        // Muting somebody changes neither the declared relation nor the session
        // map, so without this term the table would keep every route the flavor
        // declared and the flag would be pure decoration.
        let routing_recompiled = since_changed
            || rendered.audio != self.declared_audio
            || session_of != self.session_of
            || silence != self.declared_silence;
        if routing_recompiled {
            let table = compile(&rendered.audio, &session_of, &self.since, &silence);
            // `send_replace` rather than `send`: the latter reports "no reader"
            // as an error and, crucially, does **not** store the value. A shard
            // that rendered before its voice plane subscribed would then keep
            // publishing tables into a channel that still held the empty one,
            // and every route would be silently missing.
            let _previous = self.routing.send_replace(Arc::new(table));
            self.declared_audio = rendered.audio;
            self.session_of = session_of;
            self.declared_silence = silence;
        }

        let head = self.version;
        let moved_set: BTreeSet<ConnectionId> = moved.iter().copied().collect();
        let mut closed = Vec::new();
        let mut retired_channels = BTreeSet::new();
        for attached in self.connections.values_mut() {
            let overlay = rendered
                .overlays
                .get(&attached.id)
                .cloned()
                .unwrap_or_default();
            let actions = rendered
                .actions
                .get(&attached.id)
                .cloned()
                .unwrap_or_default();
            let (outcome, retired) = if moved_set.contains(&attached.id) {
                let new_see = observations
                    .get(&attached.id)
                    .copied()
                    .unwrap_or(ScopeSet::NONE);
                replan(
                    attached, &previous, &self.view, head, new_see, &overlay, &actions,
                )
            } else {
                push(attached, &self.journal, head, &overlay, &actions)
            };
            retired_channels.extend(retired);
            if outcome == Outcome::Close {
                attached.queue.mark_fatal();
                closed.push(attached.id);
            }
        }
        // Mumble clients remember every ChannelId they have removed. Since ids
        // are shard-global, one accepted removal retires that wire id for the
        // whole shard; the next render will rotate it for any peers that still
        // see the semantic channel.
        self.ids.retire_channel_ids(self.id, &retired_channels);

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
    actions: &Actions,
) -> (Outcome, BTreeSet<ChannelId>) {
    let Ok(replayed) = journal.replay(attached.cursor, head) else {
        // Below the tail: unrepairable from deltas, and dying anyway.
        return (Outcome::Close, BTreeSet::new());
    };

    let shared = filter(replayed, attached.see);
    let private = plan_elements(&attached.overlay_sent, overlay);
    let mut ops = crate::compose::splice(shared, private);
    collapse(&mut ops);

    let session = attached.session;
    let mut messages = emit(&ops, session);
    // Buttons ride with the view rather than beside it: one refusal, one retry,
    // and a menu that can never describe a turn the client did not receive.
    messages.extend(crate::emit::actions(&attached.actions_sent, actions));

    if messages.is_empty() {
        // Nothing visible changed for it. Advance the cursor anyway, otherwise
        // a connection that sees nothing change would drift off the tail of the
        // journal and be closed for no reason.
        attached.commit(head, attached.see, overlay.clone(), actions.clone());
        return (Outcome::Advanced, BTreeSet::new());
    }

    match attached.queue.try_send_all(messages) {
        Ok(()) => {
            attached.commit(head, attached.see, overlay.clone(), actions.clone());
            (Outcome::Advanced, retired_channels(&ops))
        }
        Err(Refused::Congested { .. }) => (Outcome::Congested, BTreeSet::new()),
        Err(Refused::TooLarge { .. } | Refused::Closed) => (Outcome::Close, BTreeSet::new()),
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
    actions: &Actions,
) -> (Outcome, BTreeSet<ChannelId>) {
    let from = attached.composed(before);
    let to = after.restrict(new_see).compose(overlay);

    let mut ops: Vec<PlanOp> = plan(&from, &to)
        .into_iter()
        .map(|planned| planned.op)
        .collect();
    collapse(&mut ops);

    let session = attached.session;
    let mut messages = emit(&ops, session);
    messages.extend(crate::emit::actions(&attached.actions_sent, actions));

    if messages.is_empty() {
        attached.commit(head, new_see, overlay.clone(), actions.clone());
        return (Outcome::Advanced, BTreeSet::new());
    }

    match attached.queue.try_send_all(messages) {
        // Rule 3: the triplet advances together, and only here. The guide's
        // sketch assigns the new observation before sending; doing that would
        // let a congested connection keep the new observation with the old
        // view, and the next fast-path filter would use a scope set the client
        // was never told about.
        Ok(()) => {
            attached.commit(head, new_see, overlay.clone(), actions.clone());
            (Outcome::Advanced, retired_channels(&ops))
        }
        Err(Refused::Congested { .. }) => (Outcome::Congested, BTreeSet::new()),
        Err(Refused::TooLarge { .. } | Refused::Closed) => (Outcome::Close, BTreeSet::new()),
    }
}

fn retired_channels(ops: &[PlanOp]) -> BTreeSet<ChannelId> {
    ops.iter()
        .filter_map(|op| match op {
            PlanOp::RemoveChannel(channel) => Some(*channel),
            _ => None,
        })
        .collect()
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
    let mut dirty = false;
    let mut mailbox_open = true;
    let mut next_reconcile = Instant::now();

    loop {
        if dirty && (!mailbox_open || Instant::now() >= next_reconcile) {
            mailbox_open = drain_commands(&mut shard, &mut mailbox);
            let _report = shard.reconcile();
            dirty = false;
            next_reconcile = Instant::now() + MIN_INTERVAL;
            if !mailbox_open {
                break;
            }
            continue;
        }

        if !mailbox_open {
            break;
        }

        if dirty {
            tokio::select! {
                biased;
                // Cancellation-safe: recreating Sleep with the same absolute
                // deadline neither loses nor extends the cooldown.
                () = tokio::time::sleep_until(next_reconcile) => {}
                // Cancellation-safe: `recv` consumes a command only when this
                // branch completes.
                command = mailbox.recv() => match command {
                    Some(command) => shard.handle(command),
                    None => mailbox_open = false,
                },
                // Cancellation-safe: a cancelled `Notified` returns its permit.
                // The flavor state is external, so the signal only keeps this
                // turn dirty.
                () = wake.notified() => {},
            }
        } else {
            tokio::select! {
                // Cancellation-safe: `recv` consumes a command only when this
                // branch completes.
                command = mailbox.recv() => match command {
                    Some(command) => {
                        shard.handle(command);
                        dirty = true;
                    }
                    // Every handle is gone; nothing can ever wake this shard again.
                    None => break,
                },
                // Cancellation-safe: a cancelled `Notified` returns its permit.
                () = wake.notified() => dirty = true,
            }
        }
    }

    shard
}

/// Absorb one bounded burst before publishing.
///
/// The bound guarantees that a producer which continuously refills the channel
/// cannot postpone reconciliation forever. Commands arriving after the batch
/// are still consumed during the publication cooldown.
fn drain_commands<L: ShardLogic>(
    shard: &mut Shard<L>,
    mailbox: &mut mpsc::Receiver<ShardCommand>,
) -> bool {
    for _ in 0..MAILBOX_DEPTH {
        match mailbox.try_recv() {
            Ok(command) => shard.handle(command),
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Disconnected) => return false,
        }
    }
    true
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
