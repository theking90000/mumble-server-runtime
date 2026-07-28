//! The strict simulated client judges the shard runtime over real TLS and UDP.
//!
//! The flavor is deliberately tiny and lives on the verifier side: it exposes
//! two scoped rooms and one audio domain per room. Expectations come from that
//! declared model, never from the gateway's implementation.
#![allow(clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use mumble_server_runtime_gateway::tls::Identity;
use mumble_server_runtime_gateway::{
    ConnectionIdentity, ConnectionRouter, Gateway, GatewayConfig, RouteDecision,
};
use mumble_server_runtime_shard::{
    ChannelKey, ConnectionId, DomainId, Narrow, Occupant, Reply, Scope, ScopeSet, ShardBuilder,
    ShardHandle, ShardId, ShardLogic, VoiceEvent,
};
use mumble_server_runtime_testkit::SimulatedMumbleClient;

const LEFT: ChannelKey = ChannelKey(1);
const RIGHT: ChannelKey = ChannelKey(2);
const NORMAL_TARGET: u32 = 0;
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);
const CLIENTS: usize = 4;
const ROUNDS: u64 = 25;
const LATENCY_BUDGET: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
struct Person {
    name: String,
    room: u32,
}

#[derive(Debug, Default)]
struct World {
    people: Mutex<BTreeMap<ConnectionId, Person>>,
}

impl World {
    fn record(&self, connection: ConnectionId, name: String) {
        self.guard().insert(connection, Person { name, room: 0 });
    }

    fn forget(&self, connection: ConnectionId) {
        self.guard().remove(&connection);
    }

    fn person(&self, connection: ConnectionId) -> Option<Person> {
        self.guard().get(&connection).cloned()
    }

    fn move_named(&self, name: &str, room: u32) {
        let mut people = self.guard();
        let person = people
            .values_mut()
            .find(|person| person.name == name)
            .unwrap_or_else(|| panic!("{name} must be connected"));
        person.room = room;
    }

    fn guard(&self) -> MutexGuard<'_, BTreeMap<ConnectionId, Person>> {
        self.people.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct Rooms {
    world: Arc<World>,
    connected: BTreeSet<ConnectionId>,
}

impl ShardLogic for Rooms {
    fn render(&mut self, out: &mut ShardBuilder<'_>) {
        let root = out.root("Verifier rooms");
        let left = out.channel(root, LEFT, "Left", Narrow::Into(0));
        let right = out.channel(root, RIGHT, "Right", Narrow::Into(1));
        let mut members: BTreeMap<u32, Vec<ConnectionId>> = BTreeMap::new();

        for connection in &self.connected {
            let Some(person) = self.world.person(*connection) else {
                continue;
            };
            let channel = if person.room == 0 { left } else { right };
            out.user(
                channel,
                Occupant::Connection(*connection),
                &person.name,
                Narrow::Same,
            );
            members.entry(person.room).or_default().push(*connection);
        }

        for (room, connections) in members {
            out.audio_domain(DomainId(u64::from(room)), &connections);
        }
    }

    fn observation(&mut self, connection: ConnectionId) -> ScopeSet {
        let Some(person) = self.world.person(connection) else {
            return ScopeSet::NONE;
        };
        Scope::ROOT
            .child(person.room)
            .and_then(|scope| ScopeSet::new(&[scope]).ok())
            .unwrap_or(ScopeSet::NONE)
    }

    fn observe(&mut self, event: &VoiceEvent, _out: &mut Reply) {
        match event {
            VoiceEvent::Connected { connection } => {
                self.connected.insert(*connection);
            }
            VoiceEvent::Disconnected { connection, .. } => {
                self.connected.remove(connection);
                self.world.forget(*connection);
            }
            VoiceEvent::RequestedChannel {
                connection,
                channel,
            } => {
                let room = if *channel == LEFT { 0 } else { 1 };
                if let Some(person) = self.world.guard().get_mut(connection) {
                    person.room = room;
                }
            }
            _ => {}
        }
    }
}

struct Router {
    world: Arc<World>,
    shard: ShardId,
}

impl ConnectionRouter for Router {
    async fn route(
        &self,
        connection: ConnectionId,
        identity: &ConnectionIdentity,
    ) -> RouteDecision {
        self.world.record(connection, identity.name.clone());
        RouteDecision::Attach(self.shard)
    }
}

struct TestServer {
    address: SocketAddr,
    world: Arc<World>,
    shard: ShardHandle,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl TestServer {
    async fn start(max_users: u32) -> TestServer {
        let bind: SocketAddr = "127.0.0.1:0".parse().expect("loopback address");
        let config = GatewayConfig {
            bind,
            max_users,
            ..GatewayConfig::default()
        };
        let identity = Identity::self_signed(vec!["localhost".to_owned()]).expect("test identity");
        let gateway = Gateway::bind(config, identity).await.expect("bind gateway");
        let address = gateway.address();
        let runtime = gateway.runtime();
        let world = Arc::new(World::default());
        let shard_world = Arc::clone(&world);
        let shard = runtime.create_shard(move |_handle| Rooms {
            world: shard_world,
            connected: BTreeSet::new(),
        });
        let router = Router {
            world: Arc::clone(&world),
            shard: shard.shard(),
        };
        let task = tokio::spawn(gateway.serve(router));
        TestServer {
            address,
            world,
            shard,
            task,
        }
    }

    fn move_named(&self, name: &str, room: u32) {
        self.world.move_named(name, room);
        self.shard.wake();
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn simulated_clients_replay_the_gateway_handshake_without_violation() {
    let server = TestServer::start(8).await;
    let mut alice = SimulatedMumbleClient::connect(server.address, "alice")
        .await
        .expect("alice connect");
    alice.drive_handshake().await.expect("alice handshake");

    let mut bob = SimulatedMumbleClient::connect(server.address, "bob")
        .await
        .expect("bob connect");
    bob.drive_handshake().await.expect("bob handshake");
    let bob_session = bob.self_session().expect("bob session");
    alice
        .wait_until(DELIVERY_TIMEOUT, |model| {
            model.users.contains_key(&bob_session)
        })
        .await
        .expect("alice sees bob");

    let alice_session = alice.self_session().expect("alice session");
    assert_ne!(alice_session, bob_session, "sessions must be unique");
    assert!(alice.model().channels.contains_key(&0), "root must exist");
    assert!(alice.model().users.contains_key(&alice_session));
    assert!(bob.model().users.contains_key(&alice_session));
}

#[tokio::test(flavor = "multi_thread")]
async fn scoped_views_revoke_presence_and_audio_without_reconnecting() {
    let server = TestServer::start(8).await;
    let mut alice = SimulatedMumbleClient::connect(server.address, "alice")
        .await
        .expect("alice connect");
    alice.drive_handshake().await.expect("alice handshake");
    alice
        .associate_udp(server.address, DELIVERY_TIMEOUT)
        .await
        .expect("alice UDP");

    let mut bob = SimulatedMumbleClient::connect(server.address, "bob")
        .await
        .expect("bob connect");
    bob.drive_handshake().await.expect("bob handshake");
    bob.associate_udp(server.address, DELIVERY_TIMEOUT)
        .await
        .expect("bob UDP");

    let bob_session = bob.self_session().expect("bob session");
    alice
        .wait_until(DELIVERY_TIMEOUT, |model| {
            model.users.contains_key(&bob_session)
        })
        .await
        .expect("initial shared view");

    alice
        .speak(server.address, NORMAL_TARGET, 1, &[0xA1])
        .await
        .expect("initial speech");
    assert!(
        bob.recv_voice(DELIVERY_TIMEOUT)
            .await
            .expect("voice")
            .is_some()
    );

    server.move_named("bob", 1);
    alice
        .wait_until(DELIVERY_TIMEOUT, |model| {
            !model.users.contains_key(&bob_session)
        })
        .await
        .expect("bob disappears from alice");
    bob.wait_until(DELIVERY_TIMEOUT, |model| {
        model
            .channels
            .values()
            .any(|channel| channel.name == "Right")
    })
    .await
    .expect("bob receives right room");

    alice
        .speak(server.address, NORMAL_TARGET, 2, &[0xA2])
        .await
        .expect("revoked speech");
    assert!(
        bob.recv_voice(Duration::from_millis(100))
            .await
            .expect("silence check")
            .is_none(),
        "audio crossed a scope boundary after revocation"
    );

    server.move_named("bob", 0);
    alice
        .wait_until(DELIVERY_TIMEOUT, |model| {
            model.users.contains_key(&bob_session)
        })
        .await
        .expect("bob returns without reconnecting");
}

#[tokio::test(flavor = "multi_thread")]
async fn n_clients_relay_voice_without_internal_loss_or_echo() {
    let server = TestServer::start(u32::try_from(CLIENTS + 1).expect("small client count")).await;
    let mut clients = Vec::new();
    for index in 0..CLIENTS {
        let mut client = SimulatedMumbleClient::connect(server.address, &format!("judge{index}"))
            .await
            .expect("connect");
        client.drive_handshake().await.expect("handshake");
        client
            .associate_udp(server.address, DELIVERY_TIMEOUT)
            .await
            .expect("associate UDP");
        clients.push(client);
    }

    for client in &mut clients {
        client
            .wait_until(DELIVERY_TIMEOUT, |model| model.users.len() == CLIENTS)
            .await
            .expect("complete participant view");
    }
    let sessions: Vec<u32> = clients
        .iter()
        .map(|client| client.self_session().expect("synced session"))
        .collect();
    let mut worst_latency = Duration::ZERO;
    let mut deliveries = 0_u64;

    for round in 0..ROUNDS {
        for speaker in 0..CLIENTS {
            let opus = vec![
                u8::try_from(speaker).unwrap_or(0),
                u8::try_from(round & 0xFF).unwrap_or(0),
                0xA5,
            ];
            let sent_at = Instant::now();
            clients[speaker]
                .speak(server.address, NORMAL_TARGET, round, &opus)
                .await
                .expect("speak");

            for (listener, client) in clients.iter_mut().enumerate() {
                if listener == speaker {
                    continue;
                }
                let heard = client
                    .recv_voice(DELIVERY_TIMEOUT)
                    .await
                    .expect("receiving voice")
                    .unwrap_or_else(|| {
                        panic!(
                            "internal loss: round {round}, speaker {speaker}, listener {listener}"
                        )
                    });
                worst_latency = worst_latency.max(sent_at.elapsed());
                deliveries = deliveries.saturating_add(1);
                assert_eq!(heard.opus_data, opus);
                assert_eq!(heard.sender_session, sessions[speaker]);
                assert_eq!(heard.frame_number, round);
            }

            assert!(
                clients[speaker]
                    .recv_voice(Duration::from_millis(25))
                    .await
                    .expect("echo check")
                    .is_none(),
                "normal speech was echoed to its sender"
            );
        }
    }

    assert_eq!(deliveries, ROUNDS * CLIENTS as u64 * (CLIENTS as u64 - 1));
    assert!(
        worst_latency <= LATENCY_BUDGET,
        "router latency {worst_latency:?} exceeded {LATENCY_BUDGET:?}"
    );
}
