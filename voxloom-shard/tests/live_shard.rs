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
    ActionKey, ActionTarget, ChannelId, ChannelKey, ConnectionId, DomainId, Effect, Narrow,
    Occupant, On, OutboundQueue, Reply, ScopeSet, SessionId, Shard, ShardBuilder, ShardCommand,
    ShardId, ShardLogic, ShardView, TextTarget, VoiceEvent,
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
    /// Connections placed one level below their realm rather than in it.
    squads: BTreeSet<ConnectionId>,
    label: u32,
    events: Vec<VoiceEvent>,
}

/// A channel at the root scope that nobody may write to.
const SILENT: ChannelKey = ChannelKey(300);

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

        // A door everyone sees and nobody may write into.
        let silent = out.channel(root, SILENT, "Silent", Narrow::Same);
        out.channel_can_text(silent, false);

        let mut realm_channels = BTreeMap::new();
        for realm in 0..2u32 {
            let channel = out.channel(
                root,
                ChannelKey(u64::from(100 + realm)),
                &format!("Realm {realm} {label}"),
                Narrow::Into(realm),
            );
            realm_channels.insert(realm, channel);
            let squad = out.channel(
                channel,
                ChannelKey(u64::from(200 + realm)),
                &format!("Squad {realm}"),
                Narrow::Same,
            );

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
                let placement = if self.world.squads.contains(member) {
                    squad
                } else {
                    channel
                };
                out.user(
                    placement,
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

    fn observe(&mut self, event: &VoiceEvent, out: &mut Reply) {
        self.world.events.push(event.clone());
        // The most permissive flavor there is: it carries out whatever the
        // client asked for, so what the tests below observe is the runtime's own
        // policy rather than this world's.
        if let VoiceEvent::Said {
            connection,
            to,
            text,
        } = event
        {
            out.relay(*connection, *to, text);
        }
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
        Harness::with_world(realms, capacity, World::default())
    }

    fn with_world(realms: &[(u64, u32)], capacity: usize, rest: World) -> Harness {
        let world = World {
            realms: realms
                .iter()
                .map(|(connection, realm)| (ConnectionId(*connection), *realm))
                .collect(),
            ..rest
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

    fn session(&self, connection: u64) -> u32 {
        self.shard
            .connection(ConnectionId(connection))
            .expect("attached")
            .session()
            .0
    }

    /// The wire id of a channel, as that connection knows it.
    fn channel(&self, connection: u64, name: &str) -> ChannelId {
        ChannelId(
            self.model(connection)
                .channel_id_named(name)
                .unwrap_or_else(|| panic!("connection {connection} must hold {name:?}")),
        )
    }

    /// Drive one inbound text message and collect what each connection got.
    ///
    /// Everything still passes through the strict model on its way out, so a
    /// message naming something a client does not hold fails here rather than in
    /// an assertion somebody has to think of.
    fn said(
        &mut self,
        from: u64,
        to: TextTarget,
        text: &str,
    ) -> BTreeMap<ConnectionId, Vec<ControlMessage>> {
        self.shard.handle(ShardCommand::Said {
            connection: ConnectionId(from),
            to,
            message: text.to_owned(),
        });

        let mut delivered: BTreeMap<ConnectionId, Vec<ControlMessage>> = BTreeMap::new();
        for (connection, client) in &mut self.clients {
            while let Ok(message) = client.receiver.try_recv() {
                if let Err(violation) = client.model.apply(&message) {
                    panic!("connection {connection:?}: {violation}");
                }
                delivered.entry(*connection).or_default().push(message);
            }
        }
        delivered
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

    fn observe(&mut self, event: &VoiceEvent, _out: &mut Reply) {
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
fn returning_to_a_scope_uses_fresh_channel_ids() {
    let mut harness = Harness::new(&[(1, 0), (2, 1)], 1024);
    harness.step("initial");
    let realm_zero_before = harness
        .model(1)
        .channel_id_named("Realm 0 0")
        .expect("initial realm");

    harness
        .shard
        .logic_mut()
        .world
        .realms
        .insert(ConnectionId(1), 1);
    harness.step("leave realm zero");
    assert!(harness.model(1).channel_id_named("Realm 0 0").is_none());

    harness
        .shard
        .logic_mut()
        .world
        .realms
        .insert(ConnectionId(1), 0);
    harness.step("return to realm zero");
    let realm_zero_after = harness
        .model(1)
        .channel_id_named("Realm 0 0")
        .expect("returned realm");

    assert_ne!(
        realm_zero_after, realm_zero_before,
        "a ChannelId is dead after this client accepts ChannelRemove"
    );
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
        fn observe(&mut self, _event: &VoiceEvent, _out: &mut Reply) {}
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

/// A flavor whose menu and answers are driven from the test.
struct Menu {
    offered: BTreeMap<ConnectionId, Vec<(ActionKey, String, On)>>,
    invocations: Vec<VoiceEvent>,
}

impl ShardLogic for Menu {
    fn render(&mut self, out: &mut ShardBuilder<'_>) {
        let root = out.root("Lobby");
        for connection in out.connections().to_vec() {
            out.user(
                root,
                Occupant::Connection(connection),
                &format!("player-{}", connection.0),
                Narrow::Same,
            );
        }
        for (connection, actions) in &self.offered {
            out.private(*connection, |private| {
                for (key, text, on) in actions {
                    private.action(*key, text, *on);
                }
            });
        }
    }

    fn observation(&mut self, _connection: ConnectionId) -> ScopeSet {
        ScopeSet::new(&[voxloom_shard::Scope::ROOT]).expect("one scope")
    }

    fn observe(&mut self, event: &VoiceEvent, _out: &mut Reply) {
        if matches!(event, VoiceEvent::InvokedAction { .. }) {
            self.invocations.push(event.clone());
        }
    }
}

/// A shard with two connections and one button offered to the first.
fn menu_shard() -> (
    Shard<Menu>,
    BTreeMap<ConnectionId, tokio::sync::mpsc::Receiver<ControlMessage>>,
) {
    let mut offered = BTreeMap::new();
    offered.insert(
        ConnectionId(1),
        vec![(ActionKey(1), "Start".to_owned(), On::SERVER)],
    );
    let mut shard = Shard::new(
        ShardId(4),
        Menu {
            offered,
            invocations: Vec::new(),
        },
    );

    let mut queues = BTreeMap::new();
    for connection in [ConnectionId(1), ConnectionId(2)] {
        let (queue, receiver) = OutboundQueue::with_capacity(1024);
        shard.handle(ShardCommand::attach(connection, Arc::new(queue)));
        queues.insert(connection, receiver);
    }
    (shard, queues)
}

fn menu_changes(receiver: &mut tokio::sync::mpsc::Receiver<ControlMessage>) -> Vec<(String, i32)> {
    let mut changes = Vec::new();
    while let Ok(message) = receiver.try_recv() {
        if let ControlMessage::ContextActionModify(modify) = message {
            changes.push((modify.action, modify.operation.unwrap_or_default()));
        }
    }
    changes
}

#[test]
fn an_offered_action_travels_once_and_is_withdrawn_when_it_stops_being_offered() {
    let (mut shard, mut queues) = menu_shard();

    let _first = shard.reconcile();
    let one = queues.get_mut(&ConnectionId(1)).expect("attached");
    assert_eq!(
        menu_changes(one),
        vec![("1".to_owned(), 0)],
        "the button is offered on the turn it appears"
    );
    let two = queues.get_mut(&ConnectionId(2)).expect("attached");
    assert!(
        menu_changes(two).is_empty(),
        "a private offer is private: the other connection hears nothing"
    );

    // Nothing changed: an unchanged menu must not be restated every turn.
    let _idle = shard.reconcile();
    let one = queues.get_mut(&ConnectionId(1)).expect("attached");
    assert!(menu_changes(one).is_empty(), "the offer repeated itself");

    shard.logic_mut().offered.clear();
    let _withdrawn = shard.reconcile();
    let one = queues.get_mut(&ConnectionId(1)).expect("attached");
    assert_eq!(
        menu_changes(one),
        vec![("1".to_owned(), 1)],
        "no longer rendering the action is how it is withdrawn"
    );
}

#[test]
fn an_invocation_is_checked_against_what_the_connection_was_actually_offered() {
    let (mut shard, _queues) = menu_shard();
    let _first = shard.reconcile();

    // Offered to connection 1, so connection 2 naming it reaches nothing.
    shard.handle(ShardCommand::InvokedAction {
        connection: ConnectionId(2),
        action: "1".to_owned(),
        session: None,
        channel: None,
    });
    // A name this server never wrote.
    shard.handle(ShardCommand::InvokedAction {
        connection: ConnectionId(1),
        action: "not a number".to_owned(),
        session: None,
        channel: None,
    });
    // A key nobody was offered.
    shard.handle(ShardCommand::InvokedAction {
        connection: ConnectionId(1),
        action: "99".to_owned(),
        session: None,
        channel: None,
    });
    assert!(
        shard.logic_mut().invocations.is_empty(),
        "none of these three may reach the flavor"
    );

    // The real one, with a stray selection the client attached on its own: a
    // server action is offered nowhere else, so the selection is ignored rather
    // than turning it into a user action.
    let stranger = SessionId(4_000_000);
    shard.handle(ShardCommand::InvokedAction {
        connection: ConnectionId(1),
        action: "1".to_owned(),
        session: Some(stranger),
        channel: None,
    });

    match shard.logic_mut().invocations.as_slice() {
        [
            VoiceEvent::InvokedAction {
                connection,
                action,
                on,
            },
        ] => {
            assert_eq!(*connection, ConnectionId(1));
            assert_eq!(*action, ActionKey(1));
            assert_eq!(*on, ActionTarget::Server);
        }
        other => panic!("expected exactly one invocation, got {other:?}"),
    }
}

#[test]
fn a_user_action_named_on_an_invisible_session_reaches_nothing() {
    let (mut shard, _queues) = menu_shard();
    shard.logic_mut().offered.insert(
        ConnectionId(1),
        vec![(ActionKey(1), "Poke".to_owned(), On::USER)],
    );
    let _first = shard.reconcile();

    shard.handle(ShardCommand::InvokedAction {
        connection: ConnectionId(1),
        action: "1".to_owned(),
        session: Some(SessionId(4_000_000)),
        channel: None,
    });
    assert!(
        shard.logic_mut().invocations.is_empty(),
        "an unseen session must not fall back to the server target: that would make a guessed \
         identifier tell the client whether somebody exists"
    );

    // The same action on a session it does see, which is itself.
    let visible = shard
        .connection(ConnectionId(2))
        .expect("attached")
        .session();
    shard.handle(ShardCommand::InvokedAction {
        connection: ConnectionId(1),
        action: "1".to_owned(),
        session: Some(visible),
        channel: None,
    });
    match shard.logic_mut().invocations.as_slice() {
        [VoiceEvent::InvokedAction { on, .. }] => assert_eq!(
            *on,
            ActionTarget::User(Occupant::Connection(ConnectionId(2))),
            "the flavor is told who, in its own vocabulary"
        ),
        other => panic!("expected exactly one invocation, got {other:?}"),
    }
}

#[test]
fn what_a_flavor_says_reaches_the_connection_it_named_and_nobody_else() {
    // The flavor answers one event by refusing to its author, speaking to a
    // third party, and addressing a connection that is not here at all.
    struct Chatty;

    impl ShardLogic for Chatty {
        fn render(&mut self, out: &mut ShardBuilder<'_>) {
            let root = out.root("Lobby");
            for connection in out.connections().to_vec() {
                out.user(
                    root,
                    Occupant::Connection(connection),
                    &format!("player-{}", connection.0),
                    Narrow::Same,
                );
            }
        }
        fn observation(&mut self, _connection: ConnectionId) -> ScopeSet {
            ScopeSet::new(&[voxloom_shard::Scope::ROOT]).expect("one scope")
        }
        fn observe(&mut self, event: &VoiceEvent, out: &mut Reply) {
            if let VoiceEvent::RequestedSelfState { connection, .. } = event {
                out.refuse(*connection, "not while the round is running");
                out.say(ConnectionId(2), "somebody just tried to mute themselves");
                out.say(ConnectionId(404), "into the void");
            }
        }
    }

    let mut shard = Shard::new(ShardId(3), Chatty);
    let mut queues = BTreeMap::new();
    for connection in [ConnectionId(1), ConnectionId(2)] {
        let (queue, receiver) = OutboundQueue::with_capacity(1024);
        shard.handle(ShardCommand::attach(connection, Arc::new(queue)));
        queues.insert(connection, receiver);
    }
    let _report = shard.reconcile();

    let sessions: BTreeMap<ConnectionId, SessionId> = queues
        .keys()
        .map(|connection| {
            (
                *connection,
                shard.connection(*connection).expect("attached").session(),
            )
        })
        .collect();
    for receiver in queues.values_mut() {
        while receiver.try_recv().is_ok() {}
    }

    shard.handle(ShardCommand::RequestedSelfState {
        connection: ConnectionId(1),
        self_mute: Some(true),
        self_deaf: None,
    });

    let mut delivered: BTreeMap<ConnectionId, Vec<ControlMessage>> = BTreeMap::new();
    for (connection, receiver) in &mut queues {
        while let Ok(message) = receiver.try_recv() {
            delivered.entry(*connection).or_default().push(message);
        }
    }

    match delivered.get(&ConnectionId(1)).map(Vec::as_slice) {
        Some([ControlMessage::PermissionDenied(denied)]) => {
            assert_eq!(denied.session, Some(sessions[&ConnectionId(1)].0));
            assert_eq!(
                denied.reason.as_deref(),
                Some("not while the round is running")
            );
        }
        other => panic!("the author must be refused, got {other:?}"),
    }

    match delivered.get(&ConnectionId(2)).map(Vec::as_slice) {
        Some([ControlMessage::TextMessage(text)]) => {
            assert_eq!(
                text.actor, None,
                "speech with no actor is what the client attributes to the server"
            );
            assert_eq!(
                text.session,
                vec![sessions[&ConnectionId(2)].0],
                "a message aimed at one user names that user"
            );
            assert_eq!(text.message, "somebody just tried to mute themselves");
        }
        other => panic!("the third party must be told, got {other:?}"),
    }
}

#[test]
fn a_flavor_asking_for_a_move_reaches_the_runtime_that_wired_the_shard() {
    struct Leaver;

    impl ShardLogic for Leaver {
        fn render(&mut self, out: &mut ShardBuilder<'_>) {
            let root = out.root("Lobby");
            for connection in out.connections().to_vec() {
                out.user(
                    root,
                    Occupant::Connection(connection),
                    "player",
                    Narrow::Same,
                );
            }
        }
        fn observation(&mut self, _connection: ConnectionId) -> ScopeSet {
            ScopeSet::new(&[voxloom_shard::Scope::ROOT]).expect("one scope")
        }
        fn observe(&mut self, event: &VoiceEvent, out: &mut Reply) {
            if let VoiceEvent::RequestedSelfState { connection, .. } = event {
                out.say(*connection, "Goodbye.");
                out.switch(*connection, ShardId(9));
            }
        }
    }

    let mut shard = Shard::new(ShardId(8), Leaver);
    let (queue, mut receiver) = OutboundQueue::with_capacity(1024);
    shard.handle(ShardCommand::attach(ConnectionId(1), Arc::new(queue)));
    let _report = shard.reconcile();

    let asked: Arc<std::sync::Mutex<Vec<Effect>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    shard.route_effects({
        let asked = Arc::clone(&asked);
        Arc::new(move |effect| {
            asked
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(effect);
        })
    });

    shard.handle(ShardCommand::RequestedSelfState {
        connection: ConnectionId(1),
        self_mute: Some(true),
        self_deaf: None,
    });

    assert_eq!(
        *asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![Effect::Move {
            connection: ConnectionId(1),
            to: ShardId(9)
        }]
    );

    // The farewell went out before the move was asked for, which is the whole
    // reason words are delivered first.
    let said = std::iter::from_fn(|| receiver.try_recv().ok())
        .filter(|message| matches!(message, ControlMessage::TextMessage(_)))
        .count();
    assert_eq!(said, 1, "the farewell must be on the socket already");
}

#[test]
fn a_shard_wired_to_no_runtime_drops_a_move_rather_than_pretending() {
    // A shard outside a runtime is a real state, not a missing wire: a test, a
    // benchmark. Nothing can carry the move out, so nothing may look as if it
    // had, and the connection stays exactly where it is.
    struct Leaver;

    impl ShardLogic for Leaver {
        fn render(&mut self, out: &mut ShardBuilder<'_>) {
            let root = out.root("Lobby");
            out.user(
                root,
                Occupant::Connection(ConnectionId(1)),
                "player",
                Narrow::Same,
            );
        }
        fn observation(&mut self, _connection: ConnectionId) -> ScopeSet {
            ScopeSet::new(&[voxloom_shard::Scope::ROOT]).expect("one scope")
        }
        fn observe(&mut self, event: &VoiceEvent, out: &mut Reply) {
            if let VoiceEvent::RequestedSelfState { connection, .. } = event {
                out.switch(*connection, ShardId(9));
            }
        }
    }

    let mut shard = Shard::new(ShardId(8), Leaver);
    let (queue, _receiver) = OutboundQueue::with_capacity(1024);
    shard.handle(ShardCommand::attach(ConnectionId(1), Arc::new(queue)));
    let _report = shard.reconcile();

    shard.handle(ShardCommand::RequestedSelfState {
        connection: ConnectionId(1),
        self_mute: Some(true),
        self_deaf: None,
    });

    assert!(
        shard.connection(ConnectionId(1)).is_some(),
        "an effect nobody can carry out must leave the shard untouched"
    );
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

#[test]
fn a_query_is_answered_only_about_what_the_asker_can_see() {
    // Connections 1 and 2 share realm 0; connection 3 is in realm 1 and is
    // therefore not merely filtered out of the view, but absent from it.
    let mut harness = Harness::new(&[(1, 0), (2, 0), (3, 1)], 1024);
    harness.step("initial");

    let named = |name: &str| {
        harness
            .shard
            .view()
            .channels
            .values()
            .find(|channel| channel.name == name)
            .map(|channel| channel.id)
            .expect("a rendered channel")
    };
    let mine = named("Realm 0 0");
    let theirs = named("Realm 1 0");
    let session_of = |connection: u64| {
        harness
            .shard
            .connection(ConnectionId(connection))
            .expect("attached")
            .session()
    };
    let neighbour = session_of(2);
    let stranger = session_of(3);

    for command in [
        ShardCommand::QueriedPermissions {
            connection: ConnectionId(1),
            channel: mine,
        },
        ShardCommand::QueriedPermissions {
            connection: ConnectionId(1),
            channel: theirs,
        },
        ShardCommand::QueriedUserStats {
            connection: ConnectionId(1),
            target: neighbour,
        },
        ShardCommand::QueriedUserStats {
            connection: ConnectionId(1),
            target: stranger,
        },
    ] {
        harness.shard.handle(command);
    }

    let client = harness
        .clients
        .get_mut(&ConnectionId(1))
        .expect("connection 1");
    let answers: Vec<ControlMessage> =
        std::iter::from_fn(|| client.receiver.try_recv().ok()).collect();

    // Two questions out of four are about something this connection has been
    // told exists. The other two are answered with silence, because any answer
    // at all - even a refusal - would confirm that the identifier is real.
    match answers.as_slice() {
        [
            ControlMessage::PermissionQuery(permissions),
            ControlMessage::UserStats(stats),
        ] => {
            assert_eq!(permissions.channel_id, Some(mine.0));
            assert_eq!(permissions.permissions, Some(voxloom_shard::perm::DEFAULT));
            assert_eq!(stats.session, Some(neighbour.0));
        }
        other => panic!("expected exactly the two answerable questions, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Text: the sender's view decides the target, the recipient's decides the words
// ---------------------------------------------------------------------------

#[test]
fn a_message_to_a_channel_stops_at_the_scope_boundary() {
    let mut harness = Harness::new(&[(1, 0), (2, 0), (3, 1)], 1024);
    harness.step("initial");

    let realm_zero = harness.channel(1, "Realm 0 0");
    let delivered = harness.said(1, TextTarget::Channel(realm_zero), "hold the left flank");

    match delivered.get(&ConnectionId(2)).map(Vec::as_slice) {
        Some([ControlMessage::TextMessage(text)]) => {
            assert_eq!(
                text.actor,
                Some(harness.session(1)),
                "a relayed message names who said it, or the client says the server did"
            );
            assert_eq!(text.channel_id, vec![realm_zero.0]);
            assert!(text.session.is_empty(), "this is not a private message");
            assert_eq!(text.message, "hold the left flank");
        }
        other => panic!("the realm must be told, got {other:?}"),
    }
    assert_eq!(
        delivered.get(&ConnectionId(1)),
        None,
        "a client already printed what it typed"
    );
    assert_eq!(
        delivered.get(&ConnectionId(3)),
        None,
        "the other realm is not comparable, so nothing of this reaches it"
    );
}

#[test]
fn a_message_to_a_channel_the_sender_cannot_see_is_answered_with_nothing() {
    let mut harness = Harness::new(&[(1, 0), (3, 1)], 1024);
    harness.step("initial");

    // Named through the only client that holds it, which is precisely the client
    // the sender is not.
    let realm_one = harness.channel(3, "Realm 1 0");
    let delivered = harness.said(1, TextTarget::Channel(realm_one), "are you there");

    assert!(
        delivered.is_empty(),
        "an answer that varied with whether the channel exists would be an existence oracle, got \
         {delivered:?}"
    );
}

#[test]
fn a_read_only_channel_refuses_out_loud_and_names_the_missing_right() {
    let mut harness = Harness::new(&[(1, 0), (2, 0)], 1024);
    harness.step("initial");

    let silent = harness.channel(1, "Silent");
    let delivered = harness.said(1, TextTarget::Channel(silent), "anybody home");

    match delivered.get(&ConnectionId(1)).map(Vec::as_slice) {
        Some([ControlMessage::PermissionDenied(denied)]) => {
            assert_eq!(denied.session, Some(harness.session(1)));
            assert_eq!(denied.channel_id, Some(silent.0));
            assert_eq!(denied.permission, Some(voxloom_shard::perm::TEXT_MESSAGE));
        }
        // Loud, unlike an unseen target: the client is holding this channel and
        // was already told the bit was missing, so there is nothing to leak.
        other => panic!("the writer must learn why, got {other:?}"),
    }
    assert_eq!(
        delivered.get(&ConnectionId(2)),
        None,
        "a refused message is not delivered"
    );
}

#[test]
fn a_tree_message_reaches_the_channels_below_its_root() {
    let mut harness = Harness::with_world(
        &[(1, 0), (2, 0), (3, 1)],
        1024,
        World {
            squads: BTreeSet::from([ConnectionId(2)]),
            ..World::default()
        },
    );
    harness.step("initial");

    let realm_zero = harness.channel(1, "Realm 0 0");
    let delivered = harness.said(1, TextTarget::Tree(realm_zero), "everyone in realm zero");

    match delivered.get(&ConnectionId(2)).map(Vec::as_slice) {
        Some([ControlMessage::TextMessage(text)]) => {
            assert_eq!(
                text.tree_id,
                vec![realm_zero.0],
                "the client labels a tree message differently, so the list it arrives in matters"
            );
            assert!(text.channel_id.is_empty());
        }
        other => panic!("a squad one level down is still in the tree, got {other:?}"),
    }
    assert_eq!(delivered.get(&ConnectionId(3)), None);
}

#[test]
fn a_private_message_reaches_exactly_one_connection() {
    let mut harness = Harness::new(&[(1, 0), (2, 0), (4, 0)], 1024);
    harness.step("initial");

    let target = SessionId(harness.session(2));
    let delivered = harness.said(1, TextTarget::Session(target), "psst");

    match delivered.get(&ConnectionId(2)).map(Vec::as_slice) {
        Some([ControlMessage::TextMessage(text)]) => {
            assert_eq!(text.actor, Some(harness.session(1)));
            assert_eq!(
                text.session,
                vec![target.0],
                "naming the recipient is what makes the client file it as private"
            );
        }
        other => panic!("the recipient must be told, got {other:?}"),
    }
    assert_eq!(
        delivered.get(&ConnectionId(4)),
        None,
        "a third party in the same channel is not a recipient"
    );
}

#[test]
fn a_speaker_the_audience_cannot_see_is_not_relayed_to_it() {
    let mut harness = Harness::with_world(
        &[(1, 0), (2, 0)],
        1024,
        World {
            vanished: vec![ConnectionId(1)],
            ..World::default()
        },
    );
    harness.step("initial");

    let realm_zero = harness.channel(1, "Realm 0 0");
    let delivered = harness.said(1, TextTarget::Channel(realm_zero), "I am not here");

    assert!(
        delivered.is_empty(),
        "connection 2 holds no session for a vanished speaker, so naming one would be the leak the \
         model exists to catch, got {delivered:?}"
    );
}

// ---------------------------------------------------------------------------
// Migration: the handover must survive whatever arrives before the first turn
// ---------------------------------------------------------------------------

/// One connection, two shards, and the client's model carried across the move.
///
/// Both shards share one allocator, as the runtime's do: with two of them the
/// same key would be handed the same identifier in each shard, and a migration
/// would look like a rename instead of a tree being replaced.
struct Migration {
    source: Shard<Realms>,
    destination: Shard<Realms>,
    client: Client,
    /// The sending half the connection task owns. It outlives the move, which is
    /// what makes the two shards write into one ordered stream.
    queue: Arc<OutboundQueue>,
}

impl Migration {
    fn new(connection: ConnectionId) -> Migration {
        let ids = voxloom_shard::SharedIds::new();
        let world = |label| World {
            realms: [(connection, 0)].into_iter().collect(),
            label,
            ..World::default()
        };
        let mut source = Shard::with_ids(ShardId(1), Realms { world: world(0) }, ids.clone());
        let destination = Shard::with_ids(ShardId(2), Realms { world: world(1) }, ids);

        let (queue, receiver) = OutboundQueue::with_capacity(1024);
        let queue = Arc::new(queue);
        source.handle(ShardCommand::attach(connection, Arc::clone(&queue)));
        let mut client = Client {
            receiver,
            model: ClientModel::new(),
        };
        let report = source.reconcile();
        assert!(report.refused.is_none(), "the source must render");
        client.drain("the source's first turn");

        Migration {
            source,
            destination,
            client,
            queue,
        }
    }

    /// Hand the connection over, exactly as `RuntimeHandle::move_connection`
    /// does: detach with a handover, then attach the view that came back.
    fn hand_over(&mut self, connection: ConnectionId) {
        let (view, mut awaited) = tokio::sync::oneshot::channel();
        self.source.handle(ShardCommand::Detach {
            connection,
            reason: "moving".to_owned(),
            handover: Some(voxloom_shard::Handover {
                to: ShardId(2),
                view,
            }),
        });
        let held = awaited.try_recv().expect("the source hands its view over");
        self.client.drain("what the source queued on its way out");

        self.destination.handle(ShardCommand::Attach {
            connection,
            queue: self.client_queue(),
            cursor: Arc::new(AtomicU64::new(0)),
            held,
            ready: None,
        });
    }

    /// The queue half the connection task owns, which both shards write into.
    fn client_queue(&self) -> Arc<OutboundQueue> {
        Arc::clone(&self.queue)
    }
}

#[test]
fn a_migrated_connection_is_told_to_tear_the_source_tree_down() {
    let connection = ConnectionId(1);
    let mut migration = Migration::new(connection);
    assert!(
        migration
            .client
            .model
            .channel_id_named("Realm 0 0")
            .is_some()
    );

    migration.hand_over(connection);
    let report = migration.destination.reconcile();
    assert!(report.refused.is_none(), "the destination must render");
    migration.client.drain("the destination's first turn");

    let model = &migration.client.model;
    assert!(
        model.channel_id_named("Realm 0 1").is_some(),
        "the destination's tree must have arrived"
    );
    assert_eq!(
        model.channel_id_named("Realm 0 0"),
        None,
        "the source's tree must be gone"
    );
}

#[test]
fn a_drain_before_the_first_turn_does_not_lose_the_handed_over_view() {
    let connection = ConnectionId(1);
    let mut migration = Migration::new(connection);
    migration.hand_over(connection);

    // The connection task is still flushing what the source queued, so it
    // reports its queue drained against the shard it now points at. That report
    // is about the past and must not be mistaken for a turn on this shard: the
    // fast path would find nothing to replay, commit, and drop the very view the
    // handover exists to carry.
    migration
        .destination
        .handle(ShardCommand::Drained(connection));
    let report = migration.destination.reconcile();
    assert!(report.refused.is_none(), "the destination must render");
    migration.client.drain("the destination's first turn");

    assert_eq!(
        migration.client.model.channel_id_named("Realm 0 0"),
        None,
        "the source's channels survived the migration, so the client holds two trees at once"
    );
}
