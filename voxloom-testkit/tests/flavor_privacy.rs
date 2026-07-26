//! Confidentiality of published flavor generations (roadmap P7 T7).
//!
//! The judge here is the strict [`ClientModel`]: it applies every frame the
//! runtime publishes exactly as a conforming client would and panics on any
//! reference to an entity that connection cannot see. Its checks cover every
//! leak channel of spec 26.7 (audio, presence, text, stats, blobs, plugin data,
//! targets, actors, administrative lists), so driving real generations through
//! it is what turns "the renderer looked right" into a verified property.
//!
//! Two properties are asserted, over generated snapshot pairs of the reference
//! flavor:
//!
//! 1. no published output ever names an entity absent from the receiver's view,
//!    and no member of another realm ever appears in it;
//! 2. an audio route is never wider than the receiver's committed view, at any
//!    instant of a publication — checked by streaming packets through the
//!    routing snapshot while a revocation lands, which fails if a single packet
//!    crosses between two generations.
//!
//! The generator is a self-contained xorshift PRNG (no external dependency, as
//! in the Phase 5 planner tests); the seed of a failing case is printed so any
//! counterexample is reproducible.

// The verifier reports failures by panicking with a diagnostic, so explicit
// expectations are the readable form here.
#![allow(clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use voxloom_audio::{AudioRoutingSnapshot, AudioTarget, SessionId as AudioSessionId};
use voxloom_control::{PublicationCoordinator, render_snapshot, validate_rendered_snapshot};
use voxloom_flavor::{ConnectionId, ServerPresentation, VoiceEvent, VoiceFlavor};
use voxloom_flavor_reference::{Realm, ReferenceFlavor, Snapshot};
use voxloom_protocol::ControlMessage;
use voxloom_protocol::messages::{tcp, udp};
use voxloom_render::SessionId;
use voxloom_testkit::ClientModel;

/// Deterministic PRNG (xorshift64), seeded so a counterexample can be replayed.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15 | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, bound: u32) -> u32 {
        if bound == 0 {
            0
        } else {
            u32::try_from(self.next() % u64::from(bound)).unwrap_or(0)
        }
    }
}

/// One connection's judge and the identities the harness needs to check it.
struct Connection {
    session: SessionId,
    model: ClientModel,
    /// Whether this connection has received a `ServerSync` yet. The first
    /// generation is followed by one, so invariants 1 and 6 are exercised.
    synced: bool,
}

/// A runtime composed exactly as the integration binary composes it: the
/// reference flavor on one side, the publication coordinator on the other, and
/// nothing business-shaped in between.
struct Runtime {
    flavor: ReferenceFlavor,
    coordinator: PublicationCoordinator,
    connections: BTreeMap<ConnectionId, Connection>,
    next_id: u64,
    next_session: u32,
}

impl Runtime {
    fn new() -> Runtime {
        Runtime {
            flavor: ReferenceFlavor::new("Voxloom", ServerPresentation::default()),
            coordinator: PublicationCoordinator::new(),
            connections: BTreeMap::new(),
            next_id: 1,
            next_session: 1,
        }
    }

    /// Admit one connection. Ids and sessions are monotonic and never reused,
    /// which the client model enforces on its own (invariant 12).
    fn connect(&mut self, name: &str) -> ConnectionId {
        let connection = ConnectionId::new(self.next_id);
        let session = SessionId(self.next_session);
        self.next_id += 1;
        self.next_session += 1;

        self.coordinator
            .register(connection, session)
            .expect("registering a fresh connection");
        self.connections.insert(
            connection,
            Connection {
                session,
                model: ClientModel::new(),
                synced: false,
            },
        );
        self.flavor.observe(&VoiceEvent::Connected {
            connection,
            generation: self.coordinator.generation(),
            name: name.to_owned(),
            certificate_hash: Some(format!("{name}-cert")),
        });
        connection
    }

    fn disconnect(&mut self, connection: ConnectionId) {
        self.flavor.observe(&VoiceEvent::Disconnected {
            connection,
            generation: self.coordinator.generation(),
            reason: "test departure".to_owned(),
        });
        self.coordinator.unregister(connection);
        self.connections.remove(&connection);
    }

    /// A client asking to enter a realm channel, resolved as the runtime would.
    fn request_realm(&self, connection: ConnectionId, realm: Realm) {
        self.flavor
            .observe(&VoiceEvent::ChannelInteractionRequested {
                connection,
                generation: self.coordinator.generation(),
                channel: realm.key(),
            });
    }

    /// Publish one generation and apply it to every judge.
    ///
    /// With `under_load`, one packet per speaker is routed through the current
    /// routing snapshot at every step of the publication: before the frames,
    /// between two frames of the same connection, after each commit, and after
    /// the grant. A route that outlived the view it depends on is caught right
    /// there, by the receiving model.
    fn publish(&mut self, under_load: bool) -> Arc<Snapshot> {
        let snapshot = self.flavor.snapshot();
        let connections: Vec<ConnectionId> = self.connections.keys().copied().collect();
        let rendered = render_snapshot(&self.flavor, Arc::clone(&snapshot), connections)
            .expect("the reference flavor renders every member");
        let validated =
            validate_rendered_snapshot(rendered).expect("the reference flavor validates");
        let pending = self
            .coordinator
            .publish(&validated)
            .expect("publishing a validated generation");
        let (deliveries, mut commit) = pending.split();

        self.route_if(under_load);
        for (connection, messages) in deliveries {
            for message in messages {
                match self.connections.get_mut(&connection) {
                    Some(entry) => entry.model.apply(&message),
                    None => panic!("a delivery targets connection {connection:?} we do not hold"),
                }
                self.route_if(under_load);
            }
            self.coordinator
                .commit_connection(&mut commit, connection)
                .expect("committing a delivered connection");
            self.route_if(under_load);
        }
        self.coordinator
            .finish(commit)
            .expect("closing a generation");
        self.route_if(under_load);

        // A connection that has just received its first view is synchronised,
        // which is where the model checks invariants 1 and 6.
        let pending_sync: Vec<ConnectionId> = self
            .connections
            .iter()
            .filter(|(_, entry)| !entry.synced && !entry.model.channels.is_empty())
            .map(|(connection, _)| *connection)
            .collect();
        for connection in pending_sync {
            if let Some(entry) = self.connections.get_mut(&connection) {
                let session = entry.session;
                entry
                    .model
                    .apply(&ControlMessage::ServerSync(tcp::ServerSync {
                        session: Some(session.0),
                        ..Default::default()
                    }));
                entry.synced = true;
            }
        }

        snapshot
    }

    fn route_if(&mut self, under_load: bool) {
        if under_load {
            self.route_one_packet_per_speaker();
        }
    }

    /// Send one packet from every connection through the published routing
    /// table. Each recipient's model rejects a packet from a sender it cannot
    /// see, which is the audio half of spec 26.7.
    fn route_one_packet_per_speaker(&mut self) {
        let audio: Arc<AudioRoutingSnapshot> = Arc::clone(self.coordinator.audio());
        let senders: Vec<(ConnectionId, SessionId)> = self
            .connections
            .iter()
            .map(|(connection, entry)| (*connection, entry.session))
            .collect();
        for (_sender_connection, sender) in senders {
            let recipients: Vec<AudioSessionId> = audio
                .receivers(AudioSessionId::new(sender.0), AudioTarget::Normal)
                .to_vec();
            for recipient in recipients {
                let entry = self
                    .connections
                    .values()
                    .find(|entry| entry.session.0 == recipient.get());
                match entry {
                    Some(entry) => entry.model.apply_audio(&udp::Audio {
                        sender_session: sender.0,
                        ..Default::default()
                    }),
                    None => panic!(
                        "the routing table names session {} which no connection holds",
                        recipient.get()
                    ),
                }
            }
        }
    }

    /// Every published output stays inside the receiver's realm, and every
    /// audio route stays inside the receiver's view.
    fn assert_confidentiality(&self, snapshot: &Snapshot, seed: u64) {
        let owner_of_session: BTreeMap<u32, ConnectionId> = self
            .connections
            .iter()
            .map(|(connection, entry)| (entry.session.0, *connection))
            .collect();

        for (connection, entry) in &self.connections {
            let viewer_realm = snapshot.realm_of(*connection);
            for session in entry.model.users.keys() {
                let owner = owner_of_session.get(session).copied().unwrap_or_else(|| {
                    panic!(
                        "seed {seed}: connection {connection:?} still sees session {session}, \
                         which belongs to no live connection"
                    )
                });
                assert_eq!(
                    snapshot.realm_of(owner),
                    viewer_realm,
                    "seed {seed}: connection {connection:?} sees {owner:?} from another realm"
                );
            }
        }

        let audio = self.coordinator.audio();
        for (connection, entry) in &self.connections {
            let recipients =
                audio.receivers(AudioSessionId::new(entry.session.0), AudioTarget::Normal);
            for recipient in recipients {
                let receiver = owner_of_session
                    .get(&recipient.get())
                    .copied()
                    .unwrap_or_else(|| {
                        panic!("seed {seed}: a route names session {}", recipient.get())
                    });
                let receiver_model = self
                    .connections
                    .get(&receiver)
                    .unwrap_or_else(|| panic!("seed {seed}: missing model for {receiver:?}"));
                assert!(
                    receiver_model.model.users.contains_key(&entry.session.0),
                    "seed {seed}: {receiver:?} may hear {connection:?} without seeing it"
                );
            }
        }
    }

    fn hears(&self, sender: ConnectionId, receiver: ConnectionId) -> bool {
        let (Some(sender), Some(receiver)) = (
            self.connections.get(&sender),
            self.connections.get(&receiver),
        ) else {
            return false;
        };
        self.coordinator
            .audio()
            .receivers(AudioSessionId::new(sender.session.0), AudioTarget::Normal)
            .contains(&AudioSessionId::new(receiver.session.0))
    }
}

fn realm_suffix(rng: &mut Rng) -> &'static str {
    if rng.below(2) == 0 {
        "@aurora"
    } else {
        "@borealis"
    }
}

#[test]
fn no_generation_ever_shows_or_routes_a_hidden_member() {
    for seed in 0..64u64 {
        let mut rng = Rng::new(seed);
        let mut runtime = Runtime::new();
        let mut live: Vec<ConnectionId> = Vec::new();

        for step in 0..24u32 {
            match rng.below(10) {
                0..=3 if live.len() < 6 => {
                    let name = format!("member{step}{}", realm_suffix(&mut rng));
                    live.push(runtime.connect(&name));
                }
                4..=6 if !live.is_empty() => {
                    let index = rng.below(u32::try_from(live.len()).unwrap_or(1)) as usize;
                    if let Some(connection) = live.get(index) {
                        let realm = if rng.below(2) == 0 {
                            Realm::Aurora
                        } else {
                            Realm::Borealis
                        };
                        runtime.request_realm(*connection, realm);
                    }
                }
                7 if live.len() > 1 => {
                    let index = rng.below(u32::try_from(live.len()).unwrap_or(1)) as usize;
                    if index < live.len() {
                        let connection = live.remove(index);
                        runtime.disconnect(connection);
                    }
                }
                _ => {}
            }

            if live.is_empty() {
                continue;
            }
            // Every generation is applied by the judges, which panic on any
            // invisible reference before the assertions below even run.
            let snapshot = runtime.publish(false);
            runtime.assert_confidentiality(&snapshot, seed);
        }
    }
}

#[test]
fn a_revocation_under_audio_load_never_lets_a_packet_cross() {
    let mut runtime = Runtime::new();
    let alice = runtime.connect("alice@aurora");
    let bob = runtime.connect("bob@aurora");
    let snapshot = runtime.publish(true);
    runtime.assert_confidentiality(&snapshot, 0);

    assert!(
        runtime.hears(alice, bob) && runtime.hears(bob, alice),
        "two members of one realm must hear each other before the revocation"
    );

    // Packets keep flowing while the realms are separated. Any instant at which
    // the route outlives the view is caught by the receiving model, inside
    // `publish`, not after it.
    for tick in 0..8u32 {
        runtime.route_one_packet_per_speaker();
        if tick == 4 {
            runtime.request_realm(alice, Realm::Borealis);
            let snapshot = runtime.publish(true);
            runtime.assert_confidentiality(&snapshot, 0);
        }
    }

    assert!(
        !runtime.hears(alice, bob) && !runtime.hears(bob, alice),
        "a separated member must be inaudible once the generation is published"
    );
}

#[test]
fn a_departure_stops_the_audio_of_the_member_that_left() {
    let mut runtime = Runtime::new();
    let alice = runtime.connect("alice");
    let bob = runtime.connect("bob");
    runtime.publish(true);
    assert!(runtime.hears(alice, bob));

    runtime.disconnect(bob);

    assert!(
        !runtime.hears(alice, bob),
        "a departed member must leave the routing table immediately"
    );
    let snapshot = runtime.publish(true);
    runtime.assert_confidentiality(&snapshot, 0);
}
