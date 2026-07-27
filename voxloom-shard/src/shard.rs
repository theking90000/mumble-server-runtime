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

use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio::time::Instant;

use crate::build::{BuildError, ShardBuilder};
use crate::compose::{collapse, filter};
use crate::emit::emit;
use crate::ids::{ChannelId, ChannelKey, ConnectionId, Occupant, SessionId, ShardId, SharedIds};
use crate::journal::Journal;
use crate::plan::{PlanOp, plan, plan_elements};
use crate::queue::{OutboundQueue, Refused};
use crate::routing::{AudioRelation, AudioRouting, Silence, compile};
use crate::scope::ScopeSet;
use crate::view::{Overlay, ShardView};

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
                held: Some(held),
                ready,
                queue,
                shared_cursor: cursor,
            },
        );
        self.logic.observe(&VoiceEvent::Connected { connection });
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
            // A closed receiver means the migration was abandoned between the
            // two commands; the connection is then attached nowhere, and its
            // own task tears it down.
            let _received = handover.view.send(held);
            self.logic.observe(&VoiceEvent::Migrated {
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
                Ok(()) => attached.commit(self.version, ScopeSet::NONE, Overlay::default()),
                Err(_refused) => attached.queue.mark_fatal(),
            }
        }

        self.logic.observe(&VoiceEvent::Disconnected {
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
        let visible = self
            .view
            .channels
            .get(&channel)
            .filter(|rendered| attached.see.sees(rendered.scope))
            .map(|rendered| rendered.key)
            .or_else(|| {
                attached
                    .held
                    .as_ref()
                    .and_then(|held| held.channels.get(&channel))
                    .map(|rendered| rendered.key)
            })
            .or_else(|| {
                attached
                    .overlay_sent
                    .channels
                    .get(&channel)
                    .map(|rendered| rendered.key)
            });

        match visible {
            Some(key) => self.logic.observe(&VoiceEvent::RequestedChannel {
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
        self.logic.observe(&VoiceEvent::RequestedSelfState {
            connection,
            self_mute,
            self_deaf,
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
    let from = attached.composed(before);
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
