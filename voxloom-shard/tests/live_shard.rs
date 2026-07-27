//! Build steps 6 and 7: a live shard task with connections attached.
//!
//! Step 6 asks for one shard task and one connection - loop, `reconcile`,
//! `push`, bounded queue - with the client receiving its tree and no structural
//! violation. Step 7 adds N connections on different scopes, the slow path, and
//! overlays: two clients seeing different trees, one changing scope and
//! converging **without flicker**, and a vanish appearing for exactly one.
//!
//! The connection is modelled by its queue's receiving half, which is what a
//! real connection task owns. Nothing here touches a socket, so every test is
//! deterministic without a clock.
#![allow(clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use voxloom_protocol::ControlMessage;
use voxloom_shard::{
    ChannelKey, ConnectionId, DomainId, Narrow, Occupant, OutboundQueue, ScopeSet, SessionId,
    Shard, ShardBuilder, ShardCommand, ShardId, ShardLogic, ShardView, VoiceEvent,
};

#[path = "support/model.rs"]
mod model;
use model::ClientModel;

// ---------------------------------------------------------------------------
// A two-realm world: the smallest thing with genuinely divergent views
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct World {
    /// connection -> realm.
    realms: BTreeMap<ConnectionId, u32>,
    /// Connections rendered privately to themselves instead of shared.
    vanished: Vec<ConnectionId>,
    label: u32,
    events: Vec<VoiceEvent>,
}

struct Realms {
    world: World,
}

impl Realms {
    fn realm_scope(realm: u32) -> ScopeSet {
        let scope = voxloom_shard::Scope::ROOT.child(realm).expect("depth 1");
        ScopeSet::new(&[scope]).expect("one scope")
    }
}

impl ShardLogic for Realms {
    fn render(&mut self, out: &mut ShardBuilder<'_>) {
        let root = out.root("Lobby");
        let label = self.world.label;

        let mut realm_channels = BTreeMap::new();
        for realm in 0..2u32 {
            let channel = out.channel(
                root,
                ChannelKey(u64::from(100 + realm)),
                &format!("Realm {realm} {label}"),
                Narrow::Into(realm),
            );
            realm_channels.insert(realm, channel);

            let members: Vec<ConnectionId> = self
                .world
                .realms
                .iter()
                .filter(|(connection, member_realm)| {
                    **member_realm == realm && !self.world.vanished.contains(connection)
                })
                .map(|(connection, _)| *connection)
                .collect();

            for member in &members {
                out.user(
                    channel,
                    Occupant::Connection(*member),
                    &format!("player-{}", member.0),
                    Narrow::Same,
                );
            }
            if members.len() > 1 {
                out.audio_domain(DomainId(u64::from(realm)), &members);
            }
        }

        for vanished in self.world.vanished.clone() {
            let Some(realm) = self.world.realms.get(&vanished).copied() else {
                continue;
            };
            let Some(channel) = realm_channels.get(&realm).copied() else {
                continue;
            };
            let name = format!("player-{}", vanished.0);
            out.private(vanished, |private| {
                private.user_in(channel, Occupant::Connection(vanished), &name);
            });
            // Hears its realm without being heard by it: the shape a vanished
            // staff member takes, and the reason a connection with no shared
            // presence still has to be nameable by the routing table.
            out.audio_listen(vanished, DomainId(u64::from(realm)));
        }
    }

    fn observation(&mut self, connection: ConnectionId) -> ScopeSet {
        self.world
            .realms
            .get(&connection)
            .map_or(ScopeSet::NONE, |realm| Realms::realm_scope(*realm))
    }

    fn observe(&mut self, event: &VoiceEvent) {
        self.world.events.push(event.clone());
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Client {
    receiver: tokio::sync::mpsc::Receiver<ControlMessage>,
    model: ClientModel,
}

impl Client {
    /// Drain the queue into the model, judging every message on the way.
    fn drain(&mut self, context: &str) {
        while let Ok(message) = self.receiver.try_recv() {
            if let Err(violation) = self.model.apply(&message) {
                panic!("{context}: {violation}");
            }
        }
    }
}

struct Harness {
    shard: Shard<Realms>,
    clients: BTreeMap<ConnectionId, Client>,
}

impl Harness {
    fn new(realms: &[(u64, u32)], capacity: usize) -> Harness {
        let world = World {
            realms: realms
                .iter()
                .map(|(connection, realm)| (ConnectionId(*connection), *realm))
                .collect(),
            ..World::default()
        };
        let mut shard = Shard::new(ShardId(1), Realms { world });
        let mut clients = BTreeMap::new();

        for (connection, _) in realms {
            let connection = ConnectionId(*connection);
            let (queue, receiver) = OutboundQueue::with_capacity(capacity);
            shard.handle(ShardCommand::attach(connection, Arc::new(queue)));
            clients.insert(
                connection,
                Client {
                    receiver,
                    model: ClientModel::new(),
                },
            );
        }

        Harness { shard, clients }
    }

    fn step(&mut self, context: &str) -> voxloom_shard::ReconcileReport {
        let report = self.shard.reconcile();
        assert!(report.refused.is_none(), "{context}: {:?}", report.refused);
        for (connection, client) in &mut self.clients {
            client.drain(&format!("{context}: connection {connection:?}"));
        }
        report
    }

    fn model(&self, connection: u64) -> &ClientModel {
        &self.clients[&ConnectionId(connection)].model
    }
}

// ---------------------------------------------------------------------------
// Step 6: one shard, one connection
// ---------------------------------------------------------------------------

#[test]
fn one_connection_receives_its_whole_tree_on_the_first_turn() {
    let mut harness = Harness::new(&[(1, 0)], 1024);
    let report = harness.step("initial sync");

    assert!(report.published, "the first turn must publish a version");
    assert_eq!(
        report.replanned,
        vec![ConnectionId(1)],
        "attaching is an ordinary observation change, so it takes the slow path"
    );

    // Root, plus both realm channels (the sibling realm is at /1, which is not
    // comparable to /0, so it must NOT be there).
    let model = harness.model(1);
    assert!(model.channel_id_named("Realm 0 0").is_some());
    assert_eq!(
        model.channel_id_named("Realm 1 0"),
        None,
        "the other realm is out of scope"
    );
    assert!(model.has_user(1), "the connection must see itself");
}

#[test]
fn a_connection_that_sees_no_change_still_advances_its_cursor() {
    let mut harness = Harness::new(&[(1, 0), (2, 1)], 1024);
    harness.step("initial");

    // A change confined to realm 1: connection 1 observes realm 0 and sees
    // nothing of it, but must still move forward or it would eventually fall
    // off the journal's tail and be closed for no reason.
    let before = harness
        .shard
        .connection(ConnectionId(1))
        .expect("attached")
        .cursor();

    harness
        .shard
        .logic_mut()
        .world
        .vanished
        .push(ConnectionId(2));
    let report = harness.step("a change in the other realm");

    assert!(report.published);
    let after = harness
        .shard
        .connection(ConnectionId(1))
        .expect("attached")
        .cursor();
    assert_eq!(after, harness.shard.version());
    assert!(after > before, "the cursor must not stall");
}

#[tokio::test(start_paused = true)]
async fn the_shard_task_renders_when_its_mailbox_receives_a_command() {
    let world = World {
        realms: BTreeMap::from([(ConnectionId(1), 0)]),
        ..World::default()
    };
    let shard = Shard::new(ShardId(7), Realms { world });
    let (handle, wake, mailbox) = voxloom_shard::spawn_parts(ShardId(7));

    let (queue, mut receiver) = OutboundQueue::with_capacity(1024);
    handle
        .send(ShardCommand::attach(ConnectionId(1), Arc::new(queue)))
        .expect("the mailbox has room");

    let task = tokio::spawn(voxloom_shard::run(shard, wake, mailbox));

    // The attach command alone drives one turn.
    tokio::time::advance(voxloom_shard::MIN_INTERVAL * 2).await;
    tokio::task::yield_now().await;

    let mut model = ClientModel::new();
    while let Ok(message) = receiver.try_recv() {
        model.apply(&message).expect("no structural violation");
    }
    assert!(
        model.channel_id_named("Realm 0 0").is_some(),
        "the task must have rendered and pushed without anyone calling reconcile"
    );

    drop(handle);
    tokio::time::advance(voxloom_shard::MIN_INTERVAL * 2).await;
    let shard = task.await.expect("the task ends when its handles are gone");
    assert!(shard.version() > 0);
}

struct Arrivals {
    connected: BTreeSet<ConnectionId>,
    observed: tokio::sync::watch::Sender<usize>,
}

impl ShardLogic for Arrivals {
    fn render(&mut self, out: &mut ShardBuilder<'_>) {
        let root = out.root("Burst");
        for connection in &self.connected {
            out.user(
                root,
                Occupant::Connection(*connection),
                &format!("arrival-{}", connection.0),
                Narrow::Same,
            );
        }
    }

    fn observation(&mut self, _connection: ConnectionId) -> ScopeSet {
        ScopeSet::new(&[voxloom_shard::Scope::ROOT]).unwrap_or(ScopeSet::NONE)
    }

    fn observe(&mut self, event: &VoiceEvent) {
        if let VoiceEvent::Connected { connection } = event {
            self.connected.insert(*connection);
            let _previous = self.observed.send_replace(self.connected.len());
        }
    }
}

fn queued_arrival(
    connection: ConnectionId,
) -> (
    ShardCommand,
    tokio::sync::mpsc::Receiver<ControlMessage>,
    tokio::sync::oneshot::Receiver<()>,
) {
    let (queue, receiver) = OutboundQueue::with_capacity(1024);
    let (ready, awaited) = tokio::sync::oneshot::channel();
    (
        ShardCommand::Attach {
            connection,
            queue: Arc::new(queue),
            cursor: Arc::new(AtomicU64::new(0)),
            held: ShardView::empty(),
            ready: Some(ready),
        },
        receiver,
        awaited,
    )
}

#[tokio::test(start_paused = true)]
async fn command_bursts_are_observed_immediately_and_published_at_twenty_hertz() {
    const FIRST_BURST: u64 = 200;
    const SECOND_BURST: u64 = 50;

    let (observed, mut observations) = tokio::sync::watch::channel(0);
    let shard = Shard::new(
        ShardId(8),
        Arrivals {
            connected: BTreeSet::new(),
            observed,
        },
    );
    let (handle, wake, mailbox) = voxloom_shard::spawn_parts(ShardId(8));
    let mut receivers = Vec::new();
    let mut first_ready = Vec::new();

    for raw in 1..=FIRST_BURST {
        let (command, receiver, ready) = queued_arrival(ConnectionId(raw));
        handle.send(command).expect("the burst fits the mailbox");
        receivers.push(receiver);
        first_ready.push(ready);
    }

    let task = tokio::spawn(voxloom_shard::run(shard, wake, mailbox));
    for ready in first_ready {
        ready.await.expect("the first burst is published");
    }
    assert_eq!(*observations.borrow(), 200);

    let mut second_ready = Vec::new();
    for raw in (FIRST_BURST + 1)..=(FIRST_BURST + SECOND_BURST) {
        let (command, receiver, ready) = queued_arrival(ConnectionId(raw));
        handle.send(command).expect("the cooldown burst fits");
        receivers.push(receiver);
        second_ready.push(ready);
    }

    observations
        .wait_for(|count| *count == 250)
        .await
        .expect("commands are observed during the cooldown");
    assert!(
        matches!(
            second_ready
                .first_mut()
                .expect("the second burst is not empty")
                .try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ),
        "the second publication must respect the 50 ms floor"
    );

    tokio::time::advance(voxloom_shard::MIN_INTERVAL).await;
    for ready in second_ready {
        ready.await.expect("the second burst is published");
    }

    drop(handle);
    let shard = task.await.expect("the task ends when its handles are gone");
    assert_eq!(shard.version(), 2, "each burst becomes one publication");
    assert_eq!(shard.connections().count(), 250);
    drop(receivers);
}

// ---------------------------------------------------------------------------
// Step 7: N connections, divergent scopes, replan, overlays
// ---------------------------------------------------------------------------

#[test]
fn two_connections_in_different_realms_see_different_trees() {
    let mut harness = Harness::new(&[(1, 0), (2, 1)], 1024);
    harness.step("initial");

    assert!(harness.model(1).has_user(1));
    assert!(
        !harness.model(1).has_user(2),
        "realm 0 must not see realm 1's player"
    );
    assert!(harness.model(2).has_user(2));
    assert!(!harness.model(2).has_user(1));

    assert!(harness.model(1).channel_id_named("Realm 1 0").is_none());
    assert!(harness.model(2).channel_id_named("Realm 0 0").is_none());
}

#[test]
fn changing_realm_converges_without_flickering_the_common_ancestors() {
    let mut harness = Harness::new(&[(1, 0), (2, 1)], 1024);
    harness.step("initial");

    let root_before = harness.model(1).channel_generation(0);
    let realm_zero = harness
        .model(1)
        .channel_id_named("Realm 0 0")
        .expect("its own realm");
    let realm_zero_before = harness.model(1).channel_generation(realm_zero);

    // Connection 1 moves to realm 1.
    harness
        .shard
        .logic_mut()
        .world
        .realms
        .insert(ConnectionId(1), 1);
    let report = harness.step("after the realm change");

    assert_eq!(
        report.replanned,
        vec![ConnectionId(1)],
        "only the connection whose observation moved takes the slow path"
    );

    let model = harness.model(1);
    assert!(
        model.channel_id_named("Realm 1 0").is_some(),
        "it must learn its new realm"
    );
    assert!(
        model.channel_id_named("Realm 0 0").is_none(),
        "it must forget the old one"
    );
    assert!(model.has_user(2), "and meet the player already there");

    // The root is common to both observations and must never have been torn
    // down: this is the no-flicker property, and it is what distinguishes a
    // replan from a detach followed by an attach.
    assert_eq!(model.channel_generation(0), root_before);
    // The old realm channel is genuinely gone, so its generation is too.
    assert_eq!(model.channel_generation(realm_zero), None);
    assert_ne!(realm_zero_before, None);
}

#[test]
fn a_vanished_connection_can_still_be_routed() {
    // It is absent from the shared view, which is what a vanish is - but it is
    // not absent from the runtime. Compiling the routing table from the shared
    // view alone would silently turn "hears everything, heard by nobody" into
    // "takes no part in audio", and the flavor would have no way to tell.
    let mut harness = Harness::new(&[(1, 0), (2, 0), (3, 0)], 1024);
    harness
        .shard
        .logic_mut()
        .world
        .vanished
        .push(ConnectionId(1));
    let routing = harness.shard.routing();
    harness.step("vanished from the start");

    let table = routing.borrow();
    assert!(
        table.may_hear(SessionId(2), SessionId(1)),
        "the vanished listener hears its realm"
    );
    assert!(
        !table.may_hear(SessionId(1), SessionId(2)),
        "and is heard by nobody"
    );
}

#[test]
fn a_vanish_appears_to_exactly_one_connection() {
    let mut harness = Harness::new(&[(1, 0), (2, 0), (3, 1)], 1024);
    harness.step("initial");
    assert!(harness.model(2).has_user(1), "same realm, so visible");

    harness
        .shard
        .logic_mut()
        .world
        .vanished
        .push(ConnectionId(1));
    harness.step("after vanish");

    assert!(
        harness.model(1).has_user(1),
        "a vanished connection still sees itself, through its own overlay"
    );
    assert!(
        !harness.model(2).has_user(1),
        "its realm-mate must lose it entirely"
    );
    assert!(!harness.model(3).has_user(1));
    assert!(
        !harness.shard.view().users.contains_key(&SessionId(1)),
        "and it must not be in the shared view at all"
    );

    // Unvanishing puts it back, and the realm-mate learns about it.
    harness.shard.logic_mut().world.vanished.clear();
    harness.step("after unvanish");
    assert!(harness.model(2).has_user(1));
    assert!(harness.model(1).has_user(1));
}

#[test]
fn only_the_connections_that_moved_take_the_slow_path() {
    let mut harness = Harness::new(&[(1, 0), (2, 0), (3, 1), (4, 1)], 1024);
    harness.step("initial");

    // A pure content change: nobody's observation moved.
    harness.shard.logic_mut().world.label += 1;
    let report = harness.step("a rename");
    assert!(
        report.replanned.is_empty(),
        "a rename must not replan anybody"
    );
    assert!(
        !report.routing_recompiled,
        "and it must not touch the audio plane either"
    );
    assert!(report.delta_len > 0, "but it is a real delta");
}

// ---------------------------------------------------------------------------
// Backpressure and the committed triplet
// ---------------------------------------------------------------------------

#[test]
fn a_congested_connection_commits_nothing_and_converges_after_draining() {
    // The initial sync is four messages. Give the queue eight slots and park six
    // messages in it first, so the transition fits the queue but not the free
    // space: that is congestion rather than an impossible transition.
    let world = World {
        realms: BTreeMap::from([(ConnectionId(1), 0)]),
        ..World::default()
    };
    let mut shard = Shard::new(ShardId(1), Realms { world });
    let (queue, mut receiver) = OutboundQueue::with_capacity(8);
    queue
        .try_send_all(vec![filler(); 6])
        .expect("six of eight slots");
    shard.handle(ShardCommand::attach(ConnectionId(1), Arc::new(queue)));

    let report = shard.reconcile();
    assert!(
        report.closed.is_empty(),
        "congestion is backpressure, not a fault"
    );

    let attached = shard.connection(ConnectionId(1)).expect("attached");
    assert_eq!(
        attached.cursor(),
        0,
        "nothing was accepted, so the cursor must not move"
    );
    assert_eq!(
        attached.observation(),
        ScopeSet::NONE,
        "the observation must not move either: the triplet advances together"
    );

    // An all-or-nothing admission leaves no partial transition behind: exactly
    // the six filler messages are queued, and none of the view.
    let mut queued = 0;
    while receiver.try_recv().is_ok() {
        queued += 1;
    }
    assert_eq!(
        queued, 6,
        "a refused transition must queue nothing of its own"
    );

    // Draining freed the slots. `Drained` retries this connection alone, and
    // the next turn converges on the newest desired state in one transition.
    shard.handle(ShardCommand::Drained(ConnectionId(1)));
    let report = shard.reconcile();
    assert!(report.closed.is_empty());
    assert_eq!(report.replanned, vec![ConnectionId(1)]);

    let mut model = ClientModel::new();
    while let Ok(message) = receiver.try_recv() {
        model.apply(&message).expect("no structural violation");
    }
    assert!(model.channel_id_named("Realm 0 0").is_some());
    assert!(model.has_user(1));
    assert_eq!(
        shard
            .connection(ConnectionId(1))
            .expect("attached")
            .cursor(),
        shard.version()
    );
}

/// A message that carries no view state, used only to occupy a queue slot.
fn filler() -> ControlMessage {
    ControlMessage::Ping(voxloom_protocol::messages::tcp::Ping::default())
}

#[test]
fn a_transition_larger_than_the_queue_closes_the_connection() {
    // One slot can never hold an initial sync, so retrying would be a livelock.
    let mut harness = Harness::new(&[(1, 0)], 1);
    let report = harness.shard.reconcile();

    assert_eq!(report.closed, vec![ConnectionId(1)]);
    assert!(
        harness
            .shard
            .connection(ConnectionId(1))
            .expect("still attached until the runtime detaches it")
            .must_close()
    );
}

// ---------------------------------------------------------------------------
// Audio routing
// ---------------------------------------------------------------------------

#[test]
fn a_table_published_before_anyone_subscribed_is_still_there() {
    // The voice plane subscribes when it starts, which may be after a shard has
    // already rendered. A publication that only lands when someone is listening
    // would leave the plane holding an empty table and drop every route.
    let mut harness = Harness::new(&[(1, 0), (2, 0)], 1024);
    harness.step("initial");

    let routing = harness.shard.routing();
    let table = routing.borrow();
    assert!(
        table.senders().next().is_some(),
        "the table was rendered before anyone subscribed, and lost"
    );
}

#[test]
fn the_routing_table_is_published_and_gated_on_the_receivers_cursor() {
    let mut harness = Harness::new(&[(1, 0), (2, 0)], 1024);
    let routing = harness.shard.routing();
    harness.step("initial");

    let table = routing.borrow().clone();
    let (one, two) = (SessionId(1), SessionId(2));
    assert!(table.may_hear(one, two), "same realm, so audible");
    assert!(table.may_hear(two, one));

    // The gate of guide 9.5: a receiver may only be delivered a sender's audio
    // once its cursor has reached the version that sender appeared at.
    let since = table.since(one).expect("session 1 has appeared");
    let cursor = harness
        .shard
        .connection(ConnectionId(2))
        .expect("attached")
        .cursor();
    assert!(
        cursor >= since,
        "a connection that has committed the view must pass the gate"
    );
}

#[test]
fn a_realm_change_revokes_the_route_in_the_same_turn() {
    let mut harness = Harness::new(&[(1, 0), (2, 0)], 1024);
    let routing = harness.shard.routing();
    harness.step("initial");
    assert!(routing.borrow().may_hear(SessionId(1), SessionId(2)));

    harness
        .shard
        .logic_mut()
        .world
        .realms
        .insert(ConnectionId(1), 1);
    let report = harness.step("after the realm change");

    assert!(report.routing_recompiled);
    assert!(
        !routing.borrow().may_hear(SessionId(1), SessionId(2)),
        "cutting audio early is always safe; leaving it up is not"
    );
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

#[test]
fn attaching_and_detaching_are_the_same_mechanism() {
    let mut harness = Harness::new(&[(1, 0), (2, 0)], 1024);
    harness.step("initial");
    assert!(harness.model(2).has_user(1));

    harness
        .shard
        .handle(ShardCommand::detach(ConnectionId(1), "left"));
    // The detached connection is told to tear its own view down.
    harness
        .clients
        .get_mut(&ConnectionId(1))
        .expect("a client")
        .drain("after detach");
    let torn_down = harness.model(1);
    assert!(!torn_down.has_user(1));
    assert!(torn_down.channel_id_named("Realm 0 0").is_none());

    // And the remaining connection learns it is gone on the next turn.
    harness
        .shard
        .logic_mut()
        .world
        .realms
        .remove(&ConnectionId(1));
    harness.step("after the departure is rendered");
    assert!(!harness.model(2).has_user(1));

    let events = &harness.shard.logic_mut().world.events;
    assert!(
        events.iter().any(|event| matches!(
            event,
            VoiceEvent::Disconnected { connection, .. } if *connection == ConnectionId(1)
        )),
        "the flavor must be told, and it decides what that means"
    );
}

#[test]
fn a_refused_render_keeps_the_previous_view_and_closes_nothing() {
    let mut harness = Harness::new(&[(1, 0)], 1024);
    harness.step("initial");
    let version = harness.shard.version();

    // Ask for an audio edge into a receiver that cannot see the sender: the
    // builder refuses the whole render.
    struct Broken;
    impl ShardLogic for Broken {
        fn render(&mut self, out: &mut ShardBuilder<'_>) {
            let root = out.root("Lobby");
            let realm = out.channel(root, ChannelKey(100), "Realm 0 0", Narrow::Into(0));
            out.user(
                realm,
                Occupant::Connection(ConnectionId(1)),
                "player-1",
                Narrow::Same,
            );
            // Connection 9 is rendered nowhere, so nobody can hear it.
            out.audio_edge(ConnectionId(9), ConnectionId(1));
        }
        fn observation(&mut self, _connection: ConnectionId) -> ScopeSet {
            Realms::realm_scope(0)
        }
        fn observe(&mut self, _event: &VoiceEvent) {}
    }

    let mut broken = Shard::new(ShardId(2), Broken);
    let (queue, _receiver) = OutboundQueue::with_capacity(1024);
    broken.handle(ShardCommand::attach(ConnectionId(1), Arc::new(queue)));
    let report = broken.reconcile();

    assert!(report.refused.is_some(), "the render must be refused");
    assert!(!report.published);
    assert!(
        report.closed.is_empty(),
        "a broken render is the flavor's bug, not the client's"
    );
    assert_eq!(broken.version(), 0, "the previous view is kept");
    assert_eq!(harness.shard.version(), version);
}

#[test]
fn a_self_state_request_reaches_the_flavor_and_only_for_a_connection_it_holds() {
    let mut harness = Harness::new(&[(1, 0)], 1024);
    harness.step("initial");

    harness.shard.handle(ShardCommand::RequestedSelfState {
        connection: ConnectionId(1),
        self_mute: Some(true),
        self_deaf: None,
    });
    // Never attached here: a command that raced a detach, or a client of another
    // shard. It must reach nothing.
    harness.shard.handle(ShardCommand::RequestedSelfState {
        connection: ConnectionId(99),
        self_mute: Some(true),
        self_deaf: Some(true),
    });

    let requests: Vec<VoiceEvent> = harness
        .shard
        .logic_mut()
        .world
        .events
        .iter()
        .filter(|event| matches!(event, VoiceEvent::RequestedSelfState { .. }))
        .cloned()
        .collect();

    match requests.as_slice() {
        [
            VoiceEvent::RequestedSelfState {
                connection,
                self_mute,
                self_deaf,
            },
        ] => {
            assert_eq!(*connection, ConnectionId(1));
            assert_eq!(*self_mute, Some(true));
            assert_eq!(
                *self_deaf, None,
                "a flag the client did not mention must not be invented for the flavor"
            );
        }
        other => panic!("expected exactly one request, got {other:?}"),
    }
}
