//! What the demo flavor is supposed to produce, judged on the rendered view.
//!
//! These drive the shards by hand - `handle` then `reconcile` - rather than
//! through sockets, so every assertion is about the model and none of them can
//! be flaky. The end-to-end path through TLS and UDP is exercised separately, in
//! `voxloom-gateway/tests/gateway.rs`.
#![allow(clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use voxloom_arena::arena::{Arena, Role, Side};
use voxloom_arena::directory::{Destinations, Directory, Member};
use voxloom_arena::lobby::{Choices, Intent, Lobby, LobbyUpdate};
use voxloom_gateway::Runtime;
use voxloom_shard::{
    ChannelId, ChannelKey, ConnectionId, Handover, OutboundQueue, Reply, ScopeSet, SessionId,
    Shard, ShardCommand, ShardId, ShardLogic, ShardView, VoiceEvent,
};

/// Everything the two shards share, plus a runtime handle they can migrate
/// through.
struct World {
    directory: Arc<Directory>,
    destinations: Arc<Destinations>,
    chosen: Arc<Choices>,
    _runtime: Runtime,
    runtime_handle: voxloom_gateway::RuntimeHandle,
}

impl World {
    fn new() -> World {
        let runtime = Runtime::start();
        let runtime_handle = runtime.handle();
        let destinations = Arc::new(Destinations::new());
        // The identifiers a real composition would get from `create_shard`. The
        // tests drive the shards directly, so they are stated here instead.
        destinations.set_lobby(ShardId(1));
        destinations.set_arena(ShardId(2));

        World {
            directory: Arc::new(Directory::new()),
            destinations,
            chosen: Arc::new(Choices::new()),
            _runtime: runtime,
            runtime_handle,
        }
    }

    fn join(&self, connection: u64, name: &str, staff: bool) -> ConnectionId {
        let connection = ConnectionId(connection);
        self.directory.record(
            connection,
            Member {
                name: name.to_owned(),
                staff,
            },
        );
        connection
    }

    fn arena(&self) -> Shard<Arena> {
        Shard::with_ids(
            ShardId(2),
            Arena::new(
                Arc::clone(&self.directory),
                Arc::clone(&self.destinations),
                Arc::clone(&self.chosen),
            ),
            self.runtime_handle.ids().clone(),
        )
    }

    fn lobby(&self) -> Shard<Lobby> {
        self.lobby_with_updates().1
    }

    fn lobby_with_updates(&self) -> (tokio::sync::mpsc::Sender<LobbyUpdate>, Shard<Lobby>) {
        let (updates, inbox) = tokio::sync::mpsc::channel(4);
        let lobby = Shard::with_ids(
            ShardId(1),
            Lobby::new(
                Arc::clone(&self.directory),
                Arc::clone(&self.destinations),
                Arc::clone(&self.chosen),
                inbox,
            ),
            self.runtime_handle.ids().clone(),
        );
        (updates, lobby)
    }
}

/// Attach a connection, keeping both halves of its queue alive.
///
/// The queue comes back because that is what the gateway holds: it outlives any
/// one shard, which is exactly what lets a migration hand it to the next.
fn attach<L: ShardLogic>(
    shard: &mut Shard<L>,
    connection: ConnectionId,
) -> (
    Arc<OutboundQueue>,
    tokio::sync::mpsc::Receiver<voxloom_protocol::ControlMessage>,
) {
    let (queue, receiver) = OutboundQueue::new();
    let queue = Arc::new(queue);
    shard.handle(ShardCommand::attach(connection, Arc::clone(&queue)));
    (queue, receiver)
}

/// What one connection's client would hold: the shared view it observes, plus
/// its own overlay.
fn seen<L: ShardLogic>(shard: &Shard<L>, connection: ConnectionId) -> ShardView {
    let attached = shard.connection(connection).expect("attached");
    shard
        .view()
        .restrict(attached.observation())
        .compose(attached.overlay_sent())
}

fn channel_named<L: ShardLogic>(shard: &Shard<L>, name: &str) -> Option<ChannelId> {
    shard
        .view()
        .channels
        .values()
        .find(|channel| channel.name == name)
        .map(|channel| channel.id)
}

fn names(view: &ShardView) -> BTreeSet<String> {
    view.channels
        .values()
        .map(|channel| channel.name.clone())
        .collect()
}

fn users(view: &ShardView) -> BTreeSet<String> {
    view.users.values().map(|user| user.name.clone()).collect()
}

fn session_of<L: ShardLogic>(shard: &Shard<L>, connection: ConnectionId) -> SessionId {
    shard.connection(connection).expect("attached").session()
}

/// Move a connection into a channel by name, the way a double-click does.
fn request<L: ShardLogic>(shard: &mut Shard<L>, connection: ConnectionId, channel: &str) {
    let id = channel_named(shard, channel).expect("a channel by that name");
    shard.handle(ShardCommand::Requested {
        connection,
        channel: id,
    });
    let report = shard.reconcile();
    assert!(report.refused.is_none(), "render refused: {report:?}");
}

fn settle<L: ShardLogic>(shard: &mut Shard<L>) {
    let report = shard.reconcile();
    assert!(report.refused.is_none(), "render refused: {report:?}");
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_lobby_drains_external_updates_before_rendering() {
    let world = World::new();
    let (updates, mut lobby) = world.lobby_with_updates();
    settle(&mut lobby);
    let before = lobby.version();

    updates
        .try_send(LobbyUpdate::Counter(7))
        .expect("the update channel has room");
    updates
        .try_send(LobbyUpdate::Counter(8))
        .expect("the update channel has room");
    settle(&mut lobby);

    let root = lobby
        .view()
        .channels
        .get(&ChannelId::ROOT)
        .expect("the lobby renders a root");
    assert_eq!(root.name, "Voxloom Arena | update 8");
    assert!(
        lobby.version() > before,
        "the latest external update must produce a new shared view"
    );
}

#[tokio::test]
async fn a_team_cannot_see_the_other_teams_base() {
    let world = World::new();
    let red = world.join(1, "ruby", false);
    let blue = world.join(2, "cobalt", false);
    world.chosen.set(red, Intent::Join(Side::Red));
    world.chosen.set(blue, Intent::Join(Side::Blue));

    let mut arena = world.arena();
    let _red_queue = attach(&mut arena, red);
    let _blue_queue = attach(&mut arena, blue);
    settle(&mut arena);

    let red_view = names(&seen(&arena, red));
    assert!(red_view.contains("Red Base"));
    assert!(
        !red_view.contains("Blue Base"),
        "the other team's channel is not filtered out, it is at a scope this \
         observation is not comparable to"
    );
    assert!(
        !red_view.contains("Observation Deck"),
        "players must not see where the spectators are"
    );
    // Both root-scoped channels stay reachable, which is what gives a player a
    // way back out of their own team.
    assert!(red_view.contains("Neutral Ground"));
    assert!(red_view.contains("< Back to the Lobby"));

    assert_eq!(
        users(&seen(&arena, red)),
        BTreeSet::from(["ruby".to_owned()]),
        "a red player sees red and nothing else"
    );
    assert_eq!(
        users(&seen(&arena, blue)),
        BTreeSet::from(["cobalt".to_owned()])
    );
}

#[tokio::test]
async fn a_spectator_sees_both_teams_and_is_heard_by_neither() {
    let world = World::new();
    let red = world.join(1, "ruby", false);
    let blue = world.join(2, "cobalt", false);
    let watcher = world.join(3, "iris", false);
    world.chosen.set(red, Intent::Join(Side::Red));
    world.chosen.set(blue, Intent::Join(Side::Blue));
    world.chosen.set(watcher, Intent::Spectate);

    let mut arena = world.arena();
    let _queues = (
        attach(&mut arena, red),
        attach(&mut arena, blue),
        attach(&mut arena, watcher),
    );
    settle(&mut arena);

    assert_eq!(
        users(&seen(&arena, watcher)),
        BTreeSet::from(["ruby".to_owned(), "cobalt".to_owned(), "iris".to_owned()])
    );

    let routing = arena.routing().borrow().clone();
    let (red_session, blue_session, watcher_session) = (
        session_of(&arena, red),
        session_of(&arena, blue),
        session_of(&arena, watcher),
    );

    assert!(routing.may_hear(red_session, watcher_session));
    assert!(routing.may_hear(blue_session, watcher_session));
    assert!(
        !routing.may_hear(watcher_session, red_session),
        "listening is one-way by construction, not by a rule to remember"
    );
    assert!(
        !routing.may_hear(red_session, blue_session),
        "the two teams are separate voice domains"
    );
}

#[tokio::test]
async fn an_admin_is_invisible_to_everyone_but_themselves() {
    let world = World::new();
    let red = world.join(1, "ruby", false);
    let watcher = world.join(2, "iris", false);
    let admin = world.join(3, "argus", true);
    world.chosen.set(red, Intent::Join(Side::Red));
    world.chosen.set(watcher, Intent::Spectate);

    let mut arena = world.arena();
    let _queues = (
        attach(&mut arena, red),
        attach(&mut arena, watcher),
        attach(&mut arena, admin),
    );
    settle(&mut arena);

    assert_eq!(
        arena.logic_mut().role(admin),
        Some(Role::Admin { addressing: None })
    );
    assert!(
        !arena
            .view()
            .users
            .values()
            .any(|user| user.name.contains("argus")),
        "a vanish is an absence from the shared view, not a flag on it"
    );
    assert!(
        !users(&seen(&arena, red))
            .iter()
            .any(|n| n.contains("argus"))
    );
    assert!(
        !users(&seen(&arena, watcher))
            .iter()
            .any(|n| n.contains("argus")),
        "even a spectator who sees everything shared must not see staff"
    );

    // Its own presence comes from its overlay, which is what lets the client
    // find itself when ServerSync names its session.
    let own = seen(&arena, admin);
    assert!(
        own.users
            .values()
            .any(|user| user.name == "argus (vanished)")
    );
    assert!(names(&own).contains("Overwatch"));
    assert!(
        !names(&seen(&arena, watcher)).contains("Overwatch"),
        "a private channel is private"
    );

    // Hearing everything, heard by nobody.
    let routing = arena.routing().borrow().clone();
    let admin_session = session_of(&arena, admin);
    let red_session = session_of(&arena, red);
    assert!(routing.may_hear(red_session, admin_session));
    assert!(!routing.may_hear(admin_session, red_session));
}

#[tokio::test]
async fn addressing_a_team_makes_the_admin_visible_to_exactly_that_team() {
    let world = World::new();
    let red = world.join(1, "ruby", false);
    let blue = world.join(2, "cobalt", false);
    let admin = world.join(3, "argus", true);
    world.chosen.set(red, Intent::Join(Side::Red));
    world.chosen.set(blue, Intent::Join(Side::Blue));

    let mut arena = world.arena();
    let _queues = (
        attach(&mut arena, red),
        attach(&mut arena, blue),
        attach(&mut arena, admin),
    );
    settle(&mut arena);

    // For staff, picking a base is choosing who to address.
    request(&mut arena, admin, "Red Base");
    assert_eq!(
        arena.logic_mut().role(admin),
        Some(Role::Admin {
            addressing: Some(Side::Red)
        })
    );

    assert!(
        users(&seen(&arena, red)).contains("[Staff] argus"),
        "the team being addressed must see who is speaking, or the client \
         discards the audio"
    );
    assert!(
        !users(&seen(&arena, blue)).contains("[Staff] argus"),
        "the other team learns nothing"
    );

    let routing = arena.routing().borrow().clone();
    let admin_session = session_of(&arena, admin);
    assert!(routing.may_hear(admin_session, session_of(&arena, red)));
    assert!(
        !routing.may_hear(admin_session, session_of(&arena, blue)),
        "an edge was opened to one team, not to the arena"
    );

    // Stepping onto the deck makes them vanish again, and the two go together.
    request(&mut arena, admin, "Observation Deck");
    assert!(!users(&seen(&arena, red)).contains("[Staff] argus"));
    let routing = arena.routing().borrow().clone();
    assert!(!routing.may_hear(admin_session, session_of(&arena, red)));
}

#[tokio::test]
async fn a_player_widens_and_narrows_its_own_observation() {
    let world = World::new();
    let red = world.join(1, "ruby", false);
    let blue = world.join(2, "cobalt", false);
    world.chosen.set(red, Intent::Join(Side::Red));
    world.chosen.set(blue, Intent::Join(Side::Blue));

    let mut arena = world.arena();
    let _queues = (attach(&mut arena, red), attach(&mut arena, blue));
    settle(&mut arena);
    assert!(!names(&seen(&arena, red)).contains("Blue Base"));

    // Neutral ground sits at the root scope, so every observation can reach it.
    // Stepping onto it gives up the team, and the view widens to the whole tree.
    request(&mut arena, red, "Neutral Ground");
    assert_eq!(arena.logic_mut().role(red), Some(Role::Spectator));
    let widened = seen(&arena, red);
    assert!(names(&widened).contains("Blue Base"));
    assert!(users(&widened).contains("cobalt"));

    // And picking a base narrows it again, to the other side this time.
    request(&mut arena, red, "Blue Base");
    assert_eq!(arena.logic_mut().role(red), Some(Role::Player(Side::Blue)));
    let narrowed = seen(&arena, red);
    assert!(!names(&narrowed).contains("Red Base"));
    assert!(users(&narrowed).contains("cobalt"));
}

#[tokio::test]
async fn a_migration_keeps_the_session_and_never_removes_the_client_from_itself() {
    let world = World::new();
    let player = world.join(1, "ruby", false);

    let mut lobby = world.lobby();
    let (queue, mut messages) = attach(&mut lobby, player);
    settle(&mut lobby);
    let in_lobby = session_of(&lobby, player);

    request(&mut lobby, player, "Red Team");
    assert_eq!(lobby.logic_mut().intent(player), Intent::Join(Side::Red));

    // Entering the arena is the runtime's migration, done here by hand: the
    // source hands over the view the client still holds, and pushes nothing.
    request(&mut lobby, player, "> Enter the Arena");
    assert_eq!(world.chosen.get(player), Intent::Join(Side::Red));

    while messages.try_recv().is_ok() {}
    let (view, awaited) = tokio::sync::oneshot::channel();
    lobby.handle(ShardCommand::Detach {
        connection: player,
        reason: "moving".to_owned(),
        handover: Some(Handover {
            to: ShardId(2),
            view,
        }),
    });
    let held = awaited.await.expect("the source hands the view over");
    assert!(
        names(&held).contains("Red Team"),
        "the lobby tree is what the client still holds"
    );
    // The source withdraws the buttons it offered, because the destination
    // starts from an empty registry and would otherwise leave the player with a
    // menu entry no shard will ever answer. Everything else must stay unsent: a
    // view teardown is what disconnects the official client.
    while let Ok(message) = messages.try_recv() {
        assert!(
            matches!(
                message,
                voxloom_protocol::ControlMessage::ContextActionModify(_)
            ),
            "a migration must push no view teardown, got {message:?}"
        );
    }

    let mut arena = world.arena();
    arena.handle(ShardCommand::Attach {
        connection: player,
        queue: Arc::clone(&queue),
        cursor: Arc::new(AtomicU64::new(0)),
        held,
        ready: None,
    });
    settle(&mut arena);

    assert_eq!(
        arena.logic_mut().role(player),
        Some(Role::Player(Side::Red))
    );
    assert!(names(&seen(&arena, player)).contains("Red Base"));
    assert_eq!(
        session_of(&arena, player),
        in_lobby,
        "a session that changed mid-connection would leave a ghost in the \
         client's own model forever"
    );

    // The whole reason a migration is not a detach followed by an attach: the
    // official client keeps itself in its model after a UserRemove naming its
    // own session, so the ChannelRemove that followed would read as the removal
    // of an occupied channel and it would disconnect over a protocol violation.
    let mut transition = Vec::new();
    while let Ok(message) = messages.try_recv() {
        transition.push(message);
    }
    assert!(
        !transition.iter().any(|message| matches!(
            message,
            voxloom_protocol::ControlMessage::UserRemove(removal) if removal.session == in_lobby.0
        )),
        "the migration removed the client from itself: {transition:?}"
    );
    assert!(
        transition.iter().any(|message| matches!(
            message,
            voxloom_protocol::ControlMessage::ChannelState(state)
                if state.name.as_deref() == Some("Red Base")
        )),
        "the destination has to describe its own tree: {transition:?}"
    );
}

#[tokio::test]
async fn a_channel_request_a_connection_cannot_see_is_refused() {
    let world = World::new();
    let red = world.join(1, "ruby", false);
    let blue = world.join(2, "cobalt", false);
    world.chosen.set(red, Intent::Join(Side::Red));
    world.chosen.set(blue, Intent::Join(Side::Blue));

    let mut arena = world.arena();
    let _queues = (attach(&mut arena, red), attach(&mut arena, blue));
    settle(&mut arena);

    let blue_base = channel_named(&arena, "Blue Base").expect("rendered");
    // A red player cannot see Blue Base, so guessing its number must buy
    // nothing: not the move, and not the knowledge that it exists.
    arena.handle(ShardCommand::Requested {
        connection: red,
        channel: blue_base,
    });
    settle(&mut arena);

    assert_eq!(arena.logic_mut().role(red), Some(Role::Player(Side::Red)));
}

#[tokio::test]
async fn a_disconnect_forgets_the_connection_everywhere() {
    let world = World::new();
    let player = world.join(1, "ruby", false);
    world.chosen.set(player, Intent::Join(Side::Red));

    let mut arena = world.arena();
    let queue = attach(&mut arena, player);
    settle(&mut arena);
    assert_eq!(arena.logic_mut().population(), 1);

    arena.handle(ShardCommand::detach(player, "left"));
    settle(&mut arena);

    assert_eq!(arena.logic_mut().population(), 0);
    assert_eq!(world.directory.member(player), None);
    assert_eq!(world.chosen.get(player), Intent::Undecided);
    drop(queue);
}

#[tokio::test]
async fn every_connection_observes_something_the_render_can_satisfy() {
    // The render is refused whole if a receiver cannot see its sender, or an
    // overlay points at a channel its observer cannot reach. Walking every
    // combination of roles is the cheapest way to know none of them does.
    let world = World::new();
    let people = [
        (world.join(1, "ruby", false), Intent::Join(Side::Red)),
        (world.join(2, "rose", false), Intent::Join(Side::Red)),
        (world.join(3, "cobalt", false), Intent::Join(Side::Blue)),
        (world.join(4, "iris", false), Intent::Spectate),
        (world.join(5, "argus", true), Intent::Undecided),
        (world.join(6, "vigil", true), Intent::Undecided),
    ];
    for (connection, intent) in &people {
        world.chosen.set(*connection, *intent);
    }

    let mut arena = world.arena();
    let mut queues = Vec::new();
    for (connection, _) in &people {
        queues.push(attach(&mut arena, *connection));
    }
    settle(&mut arena);

    for target in [
        "Red Base",
        "Blue Base",
        "Observation Deck",
        "Neutral Ground",
    ] {
        for (connection, _) in &people {
            let Some(id) = channel_named(&arena, target) else {
                continue;
            };
            arena.handle(ShardCommand::Requested {
                connection: *connection,
                channel: id,
            });
            let report = arena.reconcile();
            assert!(
                report.refused.is_none(),
                "{connection:?} asking for {target} produced a render the shard \
                 refused: {report:?}"
            );
        }
    }
}

#[tokio::test]
async fn private_channel_keys_never_collide_between_two_admins() {
    let world = World::new();
    let first = world.join(1, "argus", true);
    let second = world.join(2, "vigil", true);

    let mut arena = world.arena();
    let _queues = (attach(&mut arena, first), attach(&mut arena, second));
    settle(&mut arena);

    let mine: BTreeSet<ChannelId> = seen(&arena, first).channels.keys().copied().collect();
    let theirs: BTreeSet<ChannelId> = seen(&arena, second).channels.keys().copied().collect();
    let overwatch_of = |view: &ShardView| -> Option<ChannelKey> {
        view.channels
            .values()
            .find(|channel| channel.name == "Overwatch")
            .map(|channel| channel.key)
    };

    assert_ne!(
        overwatch_of(&seen(&arena, first)),
        overwatch_of(&seen(&arena, second)),
        "two admins sharing one key would share one private channel"
    );
    assert_ne!(mine, theirs);
}

#[tokio::test]
async fn an_unknown_event_variant_does_not_change_the_flavor() {
    // `VoiceEvent` is non-exhaustive: a runtime that grows a variant must not
    // silently alter what a flavor does. The catch-all logs and returns.
    let world = World::new();
    let player = world.join(1, "ruby", false);
    world.chosen.set(player, Intent::Join(Side::Red));

    let mut arena = world.arena();
    let _queue = attach(&mut arena, player);
    settle(&mut arena);

    let before = arena.logic_mut().role(player);
    arena.logic_mut().observe(
        &VoiceEvent::Migrated {
            connection: ConnectionId(999),
            to: ShardId(7),
        },
        &mut Reply::default(),
    );

    assert_eq!(arena.logic_mut().role(player), before);
}

#[tokio::test]
async fn an_observation_never_asks_for_more_scopes_than_the_runtime_holds() {
    // `ScopeSet::new` refuses past MAX_OBSERVED and this flavor falls back to
    // NONE on refusal, which would silently blind a spectator. Nothing here may
    // ever come back empty.
    let world = World::new();
    let watcher = world.join(1, "iris", false);
    let admin = world.join(2, "argus", true);
    world.chosen.set(watcher, Intent::Spectate);

    let mut arena = world.arena();
    let _queues = (attach(&mut arena, watcher), attach(&mut arena, admin));
    settle(&mut arena);

    for connection in [watcher, admin] {
        let observed = arena.logic_mut().observation(connection);
        assert_ne!(
            observed,
            ScopeSet::NONE,
            "{connection:?} would see nothing at all"
        );
    }
}
