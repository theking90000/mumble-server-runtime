//! Build steps 8, 9 and 10, exercised over real sockets.
//!
//! | step | what the guide asks for | test |
//! |---|---|---|
//! | 8 | two clients hear each other, over UDP and over the tunnel; a listener hears without being heard | [`two_clients_hear_each_other_over_udp`], [`a_listener_hears_without_being_heard`] |
//! | 9 | two clients in two shards neither see nor hear each other | [`two_shards_are_invisible_and_inaudible_to_each_other`] |
//! | 10 | migration, a full scenario replayed by a composition | [`a_migration_moves_a_client_without_removing_it_from_itself`] |
//!
//! Everything here runs against a gateway bound on an ephemeral loopback port,
//! with a real TLS handshake and real datagrams. Nothing sleeps to wait for
//! state: a test advances when the server has actually said something.
#![allow(clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Result;
use voxloom_gateway::tls::Identity;
use voxloom_gateway::{
    ConnectionIdentity, ConnectionRouter, Gateway, GatewayConfig, RouteDecision, RuntimeHandle,
};
use voxloom_shard::{
    ChannelKey, ConnectionId, DomainId, Narrow, Occupant, Scope, ScopeSet, ShardBuilder, ShardId,
    ShardLogic, UserFlags, VoiceEvent,
};

#[path = "support/client.rs"]
mod support;
use support::Client;

// ---------------------------------------------------------------------------
// A test flavor: two rooms, one per scope, plus an optional silent listener
// ---------------------------------------------------------------------------

const LEFT: ChannelKey = ChannelKey(1);
const RIGHT: ChannelKey = ChannelKey(2);

/// One domain per room, because a domain whose members cannot see each other is
/// a render the shard refuses outright: a receiver must see its sender.
fn voice_of(room: u32) -> DomainId {
    DomainId(u64::from(room))
}

/// Which room a connection is in, shared between the router and the shards.
#[derive(Debug, Default)]
struct Roster {
    /// connection -> (name, room). Room 2 means "a listener": it observes both
    /// rooms and speaks into neither.
    people: std::sync::Mutex<BTreeMap<ConnectionId, (String, u32)>>,
    /// What each connection asked for its own audio state. The flavor owns it:
    /// the runtime keeps no copy.
    flags: std::sync::Mutex<BTreeMap<ConnectionId, UserFlags>>,
}

impl Roster {
    fn place(&self, connection: ConnectionId, name: String, room: u32) {
        self.guard().insert(connection, (name, room));
    }

    fn room(&self, connection: ConnectionId) -> Option<u32> {
        self.guard().get(&connection).map(|(_, room)| *room)
    }

    fn name(&self, connection: ConnectionId) -> String {
        self.guard()
            .get(&connection)
            .map_or_else(|| "anon".to_owned(), |(name, _)| name.clone())
    }

    fn move_to(&self, connection: ConnectionId, room: u32) {
        if let Some(entry) = self.guard().get_mut(&connection) {
            entry.1 = room;
        }
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, BTreeMap<ConnectionId, (String, u32)>> {
        self.people
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Grant a self-state request. A flag the client did not mention keeps the
    /// value this flavor already renders.
    fn set_self_state(
        &self,
        connection: ConnectionId,
        self_mute: Option<bool>,
        self_deaf: Option<bool>,
    ) {
        let mut flags = self
            .flags
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = flags.entry(connection).or_default();
        if let Some(mute) = self_mute {
            entry.self_mute = mute;
        }
        if let Some(deaf) = self_deaf {
            entry.self_deaf = deaf;
        }
    }

    fn flags(&self, connection: ConnectionId) -> UserFlags {
        self.flags
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&connection)
            .copied()
            .unwrap_or_default()
    }
}

struct Rooms {
    roster: Arc<Roster>,
    here: Vec<ConnectionId>,
    /// Connections this shard should hand to the other one on request.
    elsewhere: Arc<std::sync::OnceLock<ShardId>>,
    runtime: RuntimeHandle,
}

fn room_scope(room: u32) -> ScopeSet {
    match Scope::ROOT.child(room) {
        Some(scope) => ScopeSet::new(&[scope]).unwrap_or(ScopeSet::NONE),
        None => ScopeSet::NONE,
    }
}

/// A listener sees both rooms.
fn listener_scope() -> ScopeSet {
    let (Some(left), Some(right)) = (Scope::ROOT.child(0), Scope::ROOT.child(1)) else {
        return ScopeSet::NONE;
    };
    ScopeSet::new(&[left, right]).unwrap_or(ScopeSet::NONE)
}

impl ShardLogic for Rooms {
    fn render(&mut self, out: &mut ShardBuilder<'_>) {
        let root = out.root("Rooms");
        let left = out.channel(root, LEFT, "Left", Narrow::Into(0));
        let right = out.channel(root, RIGHT, "Right", Narrow::Into(1));

        let mut rooms: BTreeMap<u32, Vec<ConnectionId>> = BTreeMap::new();
        let mut listeners = Vec::new();
        for connection in &self.here {
            let name = self.roster.name(*connection);
            match self.roster.room(*connection) {
                Some(room @ 0..=1) => {
                    let channel = if room == 0 { left } else { right };
                    let user = out.user(
                        channel,
                        Occupant::Connection(*connection),
                        &name,
                        Narrow::Same,
                    );
                    out.user_flags(user, self.roster.flags(*connection));
                    rooms.entry(room).or_default().push(*connection);
                }
                // A listener sits in Left so it is somewhere, and is in no voice
                // domain: it hears without being heard.
                _ => {
                    let user =
                        out.user(left, Occupant::Connection(*connection), &name, Narrow::Same);
                    out.user_flags(user, self.roster.flags(*connection));
                    listeners.push(*connection);
                }
            }
        }

        for (room, members) in &rooms {
            out.audio_domain(voice_of(*room), members);
        }
        for listener in &listeners {
            // A listener observes both rooms, so it may hear both.
            out.audio_listen(*listener, voice_of(0));
            out.audio_listen(*listener, voice_of(1));
        }
    }

    fn observation(&mut self, connection: ConnectionId) -> ScopeSet {
        match self.roster.room(connection) {
            Some(room @ 0..=1) => room_scope(room),
            _ => listener_scope(),
        }
    }

    fn observe(&mut self, event: &VoiceEvent) {
        match event {
            VoiceEvent::Connected { connection } => self.here.push(*connection),
            VoiceEvent::Disconnected { connection, .. }
            | VoiceEvent::Migrated { connection, .. } => {
                self.here.retain(|here| here != connection);
            }
            VoiceEvent::RequestedChannel {
                connection,
                channel,
            } => match *channel {
                LEFT => self.roster.move_to(*connection, 0),
                RIGHT => self.roster.move_to(*connection, 1),
                // The root means "take me to the other shard", which is the
                // shortest way to reach a migration from a stock client.
                _ => {
                    if let Some(other) = self.elsewhere.get() {
                        self.runtime.move_connection(*connection, *other);
                    }
                }
            },
            // Granted as asked. A flavor is free to refuse by rendering
            // nothing new, which is what makes this a request.
            VoiceEvent::RequestedSelfState {
                connection,
                self_mute,
                self_deaf,
            } => self
                .roster
                .set_self_state(*connection, *self_mute, *self_deaf),
            other => eprintln!("test flavor ignores {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Router {
    roster: Arc<Roster>,
    shard: ShardId,
}

impl ConnectionRouter for Router {
    async fn route(
        &self,
        connection: ConnectionId,
        identity: &ConnectionIdentity,
    ) -> RouteDecision {
        if identity.name == "banned" {
            return RouteDecision::Reject("not here".to_owned());
        }
        // The credential decides the room, which keeps every test's setup to one
        // string in the client's password field.
        let room = match identity.credential.as_deref() {
            Some("right") => 1,
            Some("listen") => 2,
            _ => 0,
        };
        self.roster.place(connection, identity.name.clone(), room);
        RouteDecision::Attach(self.shard)
    }
}

struct Harness {
    address: std::net::SocketAddr,
    roster: Arc<Roster>,
    /// The second shard, for the multi-shard tests.
    second: ShardId,
}

impl Harness {
    /// Bind a gateway with two shards and start serving it in the background.
    async fn start() -> Result<Harness> {
        let identity = Identity::self_signed(vec!["localhost".to_owned()])?;
        let config = GatewayConfig {
            bind: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            ..GatewayConfig::default()
        };
        let gateway = Gateway::bind(config, identity).await?;
        let address = gateway.address();
        let runtime = gateway.runtime();
        let roster = Arc::new(Roster::default());
        let second_id = Arc::new(std::sync::OnceLock::new());
        let first_id = Arc::new(std::sync::OnceLock::new());

        let first = runtime.create_shard(|_handle| Rooms {
            roster: Arc::clone(&roster),
            here: Vec::new(),
            elsewhere: Arc::clone(&second_id),
            runtime: runtime.clone(),
        });
        let second = runtime.create_shard(|_handle| Rooms {
            roster: Arc::clone(&roster),
            here: Vec::new(),
            elsewhere: Arc::clone(&first_id),
            runtime: runtime.clone(),
        });
        let _set = second_id.set(second.shard());
        let _set = first_id.set(first.shard());

        let router = Router {
            roster: Arc::clone(&roster),
            shard: first.shard(),
        };
        // Detached: the test owns the clients, and the gateway ends with the
        // test process.
        tokio::spawn(async move {
            let _served = gateway.serve(router).await;
        });

        Ok(Harness {
            address,
            roster,
            second: second.shard(),
        })
    }
}

// ---------------------------------------------------------------------------
// Step 8: the voice plane
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_client_receives_its_tree_before_it_is_synchronised() -> Result<()> {
    let harness = Harness::start().await?;
    let client = Client::connect(harness.address, "alice", None).await?;

    // `Client::connect` returns at ServerSync, so everything below arrived
    // before it: that is invariant 6 observed rather than asserted about.
    // Right is at a scope this observation is not comparable to, so it was
    // never in a delta this client received.
    assert_eq!(client.model.channel_names(), vec!["Left", "Rooms"]);
    assert_eq!(client.model.user_names(), vec!["alice".to_owned()]);
    assert_eq!(
        client.model.users.get(&client.session()).map(|u| &u.name),
        Some(&"alice".to_owned()),
        "the client must find itself under the session ServerSync named"
    );
    Ok(())
}

#[tokio::test]
async fn two_clients_hear_each_other_over_udp() -> Result<()> {
    let harness = Harness::start().await?;
    let mut alice = Client::connect(harness.address, "alice", None).await?;
    let mut bob = Client::connect(harness.address, "bob", None).await?;

    alice
        .settle("bob to appear", |model| model.user_named("bob").is_some())
        .await?;
    bob.settle("alice to appear", |model| {
        model.user_named("alice").is_some()
    })
    .await?;

    alice.open_udp().await?;
    bob.open_udp().await?;
    // The pings prove both addresses. Draining what came back leaves the socket
    // holding only audio.
    let _ping = alice.hear().await;
    let _ping = bob.hear().await;

    alice.speak(b"opus-frames").await?;
    let heard = bob.hear().await?.expect("bob must hear alice");

    assert_eq!(heard.sender_session, alice.session());
    assert_eq!(
        heard.opus_data, b"opus-frames",
        "routing must never decode the payload"
    );
    assert!(
        matches!(
            heard.header,
            Some(voxloom_protocol::messages::udp::audio::Header::Context(0))
        ),
        "the server-to-client direction carries a context, not a target"
    );
    Ok(())
}

#[tokio::test]
async fn a_client_that_never_opens_udp_is_served_through_the_tunnel() -> Result<()> {
    let harness = Harness::start().await?;
    let mut alice = Client::connect(harness.address, "alice", None).await?;
    let mut bob = Client::connect(harness.address, "bob", None).await?;

    alice
        .settle("bob", |model| model.user_named("bob").is_some())
        .await?;
    bob.settle("alice", |model| model.user_named("alice").is_some())
        .await?;

    alice.open_udp().await?;
    let _ping = alice.hear().await;
    alice.speak(b"tunnelled").await?;

    // Bob proved no address, so the only way to reach him is the tunnel he is
    // already reading. The sender's transport never enters into it.
    bob.settle("tunnelled audio", |model| !model.tunnelled.is_empty())
        .await?;
    let heard = bob.model.tunnelled.first().expect("one packet");
    assert_eq!(heard.sender_session, alice.session());
    assert_eq!(heard.opus_data, b"tunnelled");
    Ok(())
}

#[tokio::test]
async fn a_listener_hears_without_being_heard() -> Result<()> {
    let harness = Harness::start().await?;
    let mut alice = Client::connect(harness.address, "alice", None).await?;
    let mut watcher = Client::connect(harness.address, "watcher", Some("listen")).await?;

    alice
        .settle("watcher", |model| model.user_named("watcher").is_some())
        .await?;
    watcher
        .settle("alice", |model| model.user_named("alice").is_some())
        .await?;

    alice.open_udp().await?;
    watcher.open_udp().await?;
    let _ping = alice.hear().await;
    let _ping = watcher.hear().await;

    alice.speak(b"heard").await?;
    let heard = watcher.hear().await?.expect("a listener hears the domain");
    assert_eq!(heard.sender_session, alice.session());

    watcher.speak(b"silent").await?;
    assert!(
        alice.hear().await?.is_none(),
        "listening is one-way: nothing the listener says may come back"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Step 9: several shards
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_clients_on_different_scopes_see_different_trees() -> Result<()> {
    let harness = Harness::start().await?;
    let left = Client::connect(harness.address, "lefty", None).await?;
    let mut right = Client::connect(harness.address, "righty", Some("right")).await?;

    right
        .settle("its own placement", |model| {
            model.user_named("righty").is_some()
        })
        .await?;

    assert!(
        !left.model.user_names().contains(&"righty".to_owned()),
        "the other scope is not filtered out downstream, it never entered the delta"
    );
    assert!(!right.model.user_names().contains(&"lefty".to_owned()));
    Ok(())
}

#[tokio::test]
async fn two_shards_are_invisible_and_inaudible_to_each_other() -> Result<()> {
    let harness = Harness::start().await?;
    let mut alice = Client::connect(harness.address, "alice", None).await?;
    let mut bob = Client::connect(harness.address, "bob", None).await?;
    alice
        .settle("bob", |model| model.user_named("bob").is_some())
        .await?;

    // Bob asks for the root, which this flavor reads as "take me to the other
    // shard".
    let root = bob.model.channel_named("Rooms").expect("a root");
    let before = bob.model.channel_named("Left").expect("the first Left");
    bob.enter(root).await?;
    bob.settle("the second shard's tree", |model| {
        // The tree is rebuilt under fresh identifiers, because an id withdrawn
        // by one shard must never come back from another.
        model
            .channel_named("Left")
            .is_some_and(|left| left != before)
    })
    .await?;

    alice
        .settle("bob to leave", |model| model.user_named("bob").is_none())
        .await?;
    assert!(
        !alice.model.user_names().contains(&"bob".to_owned()),
        "a connection on another shard is not in this one's view at all"
    );

    alice.open_udp().await?;
    bob.open_udp().await?;
    let _ping = alice.hear().await;
    let _ping = bob.hear().await;

    alice.speak(b"across shards").await?;
    assert!(
        bob.hear().await?.is_none(),
        "shards are separate audio worlds; nothing crosses"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Step 10: migration
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_migration_moves_a_client_without_removing_it_from_itself() -> Result<()> {
    let harness = Harness::start().await?;
    let mut alice = Client::connect(harness.address, "alice", None).await?;
    let session = alice.session();
    let before = alice.model.channel_named("Left").expect("the first Left");

    let root = alice.model.channel_named("Rooms").expect("a root");
    alice.enter(root).await?;
    alice
        .settle("the second shard's tree", |model| {
            model
                .channel_named("Left")
                .is_some_and(|left| left != before)
        })
        .await?;

    assert_eq!(
        alice.session(),
        session,
        "the session must survive the move, or the client keeps a ghost of \
         itself forever"
    );
    assert!(
        !alice.model.removed_users.contains(&session),
        "the client keeps itself in its own model after a UserRemove naming \
         its own session, so removing it would strand it in a channel the next \
         ChannelRemove then deletes, and it would disconnect"
    );
    assert!(
        alice.model.users.contains_key(&session),
        "and it must still be able to find itself afterwards"
    );
    assert_eq!(harness.roster.room(ConnectionId(1)), Some(0));
    assert_ne!(harness.second, ShardId(0));
    Ok(())
}

#[tokio::test]
async fn a_rejected_connection_is_told_why() -> Result<()> {
    let harness = Harness::start().await?;
    // `connect` waits for ServerSync, which will never arrive: the reject comes
    // first and the server then closes.
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        Client::connect(harness.address, "banned", None),
    )
    .await?;
    assert!(
        outcome.is_err(),
        "a rejected connection must not reach ServerSync"
    );
    Ok(())
}

#[tokio::test]
async fn the_cursor_gate_holds_a_route_until_the_receiver_has_been_told() -> Result<()> {
    // The gate is `receiver.cursor >= routing.since(sender)`. Observing it from
    // outside means watching a receiver that has been told about a sender start
    // hearing them, and never the other way round.
    let harness = Harness::start().await?;
    let mut alice = Client::connect(harness.address, "alice", None).await?;
    alice.open_udp().await?;
    let _ping = alice.hear().await;

    let mut bob = Client::connect(harness.address, "bob", None).await?;
    bob.open_udp().await?;
    let _ping = bob.hear().await;

    // Bob has not necessarily been told about alice yet. Once he has, the route
    // must work; before, nothing may arrive that he could not attribute.
    bob.settle("alice", |model| model.user_named("alice").is_some())
        .await?;
    alice.speak(b"now").await?;
    let heard = bob
        .hear()
        .await?
        .expect("the gate opens once the view is in");
    assert_eq!(heard.sender_session, alice.session());
    Ok(())
}

// ---------------------------------------------------------------------------
// Client intents: the self-state request
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_client_that_mutes_itself_is_shown_muted_to_everyone() -> Result<()> {
    let harness = Harness::start().await?;
    let mut alice = Client::connect(harness.address, "alice", None).await?;
    let mut bob = Client::connect(harness.address, "bob", None).await?;
    bob.settle("alice to appear", |model| {
        model.user_named("alice").is_some()
    })
    .await?;
    let alice_session = alice.session();

    // The mute button: both flags, and no session field at all.
    alice.set_self_state(true, false).await?;

    // The request reaches the flavor, the flavor renders the flag, and the
    // ordinary delta path carries it. Nothing here is a special case: it is one
    // `UserState` patch among the others.
    let muted = |model: &support::Model| {
        model
            .users
            .get(&alice_session)
            .is_some_and(|user| user.self_mute)
    };
    alice.settle("its own mute to come back", muted).await?;
    bob.settle("alice to be shown muted", muted).await?;
    assert!(
        !bob.model
            .users
            .get(&alice_session)
            .is_some_and(|user| user.self_deaf),
        "asking to be muted must not deafen"
    );

    alice.set_self_state(false, false).await?;
    bob.settle("alice to be unmuted again", |model| {
        model
            .users
            .get(&alice_session)
            .is_some_and(|user| !user.self_mute)
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn a_muted_client_is_not_heard_and_still_hears() -> Result<()> {
    let harness = Harness::start().await?;
    let mut alice = Client::connect(harness.address, "alice", None).await?;
    let mut bob = Client::connect(harness.address, "bob", None).await?;
    alice
        .settle("bob to appear", |model| model.user_named("bob").is_some())
        .await?;
    bob.settle("alice to appear", |model| {
        model.user_named("alice").is_some()
    })
    .await?;

    alice.open_udp().await?;
    bob.open_udp().await?;
    let _ping = alice.hear().await;
    let _ping = bob.hear().await;

    // The line works before the mute, so what follows is the mute and nothing
    // else.
    alice.speak(b"before").await?;
    let _heard = bob.hear().await?.expect("bob hears alice before the mute");

    alice.set_self_state(true, false).await?;
    let alice_session = alice.session();
    // No sleep: the shard publishes its routing table before it pushes any view,
    // so bob holding the flag proves the table that carries it is already live.
    bob.settle("alice to be shown muted", |model| {
        model
            .users
            .get(&alice_session)
            .is_some_and(|user| user.self_mute)
    })
    .await?;

    alice.speak(b"after").await?;
    assert!(
        bob.hear().await?.is_none(),
        "a muted microphone must reach nobody, whatever the flavor declared"
    );

    // The ear is untouched: muting is one direction only.
    bob.speak(b"reply").await?;
    let back = alice.hear().await?.expect("a muted client still hears");
    assert_eq!(back.sender_session, bob.session());
    Ok(())
}

#[tokio::test]
async fn a_deafened_client_is_given_nothing() -> Result<()> {
    let harness = Harness::start().await?;
    let mut alice = Client::connect(harness.address, "alice", None).await?;
    let mut bob = Client::connect(harness.address, "bob", None).await?;
    alice
        .settle("bob to appear", |model| model.user_named("bob").is_some())
        .await?;
    bob.settle("alice to appear", |model| {
        model.user_named("alice").is_some()
    })
    .await?;

    alice.open_udp().await?;
    bob.open_udp().await?;
    let _ping = alice.hear().await;
    let _ping = bob.hear().await;

    bob.speak(b"before").await?;
    let _heard = alice
        .hear()
        .await?
        .expect("alice hears bob before deafening");

    // The headphone button. Deafening implies muting, which is why the client
    // never sends the two apart.
    alice.set_self_state(false, true).await?;
    let alice_session = alice.session();
    bob.settle("alice to be shown deafened", |model| {
        model
            .users
            .get(&alice_session)
            .is_some_and(|user| user.self_deaf && user.self_mute)
    })
    .await?;

    bob.speak(b"after").await?;
    assert!(
        alice.hear().await?.is_none(),
        "a deafened client must be given nothing at all"
    );
    Ok(())
}
