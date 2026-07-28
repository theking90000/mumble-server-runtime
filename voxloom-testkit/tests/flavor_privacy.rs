//! Verifier-side oracle for the shard runtime.
//!
//! After every publication, the strict client model must equal the scoped
//! shared view composed with the connection's committed overlay. The generator
//! interleaves all four change classes from the implementation guide.
#![allow(clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::mpsc;
use voxloom_protocol::ControlMessage;
use voxloom_shard::{
    ChannelKey, ConnectionId, DomainId, Narrow, Occupant, OutboundQueue, Reply, Scope, ScopeSet,
    Shard, ShardBuilder, ShardCommand, ShardId, ShardLogic, VoiceEvent,
};
use voxloom_testkit::ClientModel;

const TEAMS: u32 = 2;

#[derive(Debug, Clone)]
struct Player {
    connection: ConnectionId,
    team: u32,
    vanished: bool,
}

#[derive(Debug, Clone, Default)]
struct World {
    players: Vec<Player>,
    label: u32,
}

impl World {
    fn team_of(&self, connection: ConnectionId) -> Option<u32> {
        self.players
            .iter()
            .find(|player| player.connection == connection)
            .map(|player| player.team)
    }

    fn visible_members(&self, team: u32) -> Vec<ConnectionId> {
        self.players
            .iter()
            .filter(|player| player.team == team && !player.vanished)
            .map(|player| player.connection)
            .collect()
    }
}

struct TestLogic {
    world: World,
}

impl ShardLogic for TestLogic {
    fn render(&mut self, out: &mut ShardBuilder<'_>) {
        let root = out.root("Lobby");
        let label = self.world.label;
        let game = out.channel(
            root,
            ChannelKey(1),
            &format!("Game {label}"),
            Narrow::Into(1),
        );
        let mut team_channels = BTreeMap::new();

        for team in 0..TEAMS {
            let members = self.world.visible_members(team);
            if members.is_empty() {
                continue;
            }
            let channel = out.channel(
                game,
                ChannelKey(u64::from(10 + team)),
                &format!("Team {team} {label}"),
                Narrow::Into(team),
            );
            team_channels.insert(team, channel);
            for member in &members {
                out.user(
                    channel,
                    Occupant::Connection(*member),
                    &format!("player-{}", member.0),
                    Narrow::Same,
                );
            }
            out.audio_domain(DomainId(u64::from(team)), &members);
        }

        for player in self.world.players.clone() {
            if !player.vanished {
                continue;
            }
            let target = team_channels.get(&player.team).copied().unwrap_or(game);
            out.private(player.connection, |private| {
                private.user_in(
                    target,
                    Occupant::Connection(player.connection),
                    &format!("player-{}", player.connection.0),
                );
            });
            if team_channels.contains_key(&player.team) {
                out.audio_listen(player.connection, DomainId(u64::from(player.team)));
            }
        }
    }

    fn observation(&mut self, connection: ConnectionId) -> ScopeSet {
        let Some(team) = self.world.team_of(connection) else {
            return ScopeSet::NONE;
        };
        Scope::ROOT
            .child(1)
            .and_then(|game| game.child(team))
            .and_then(|scope| ScopeSet::new(&[scope]).ok())
            .unwrap_or(ScopeSet::NONE)
    }

    fn observe(&mut self, _event: &VoiceEvent, _out: &mut Reply) {}
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        usize::try_from(self.next() % bound as u64).unwrap_or(0)
    }
}

struct Harness {
    shard: Shard<TestLogic>,
    models: BTreeMap<ConnectionId, ClientModel>,
    receivers: BTreeMap<ConnectionId, mpsc::Receiver<ControlMessage>>,
}

impl Harness {
    fn new(players: usize) -> Harness {
        let world = World {
            players: (0..players)
                .map(|index| Player {
                    connection: ConnectionId(index as u64 + 1),
                    team: (index as u32) % TEAMS,
                    vanished: false,
                })
                .collect(),
            label: 0,
        };
        let mut shard = Shard::new(ShardId(1), TestLogic { world });
        let mut models = BTreeMap::new();
        let mut receivers = BTreeMap::new();

        for index in 0..players {
            let connection = ConnectionId(index as u64 + 1);
            let (queue, receiver) = OutboundQueue::with_capacity(4096);
            shard.handle(ShardCommand::attach(connection, Arc::new(queue)));
            models.insert(connection, ClientModel::new());
            receivers.insert(connection, receiver);
        }
        Harness {
            shard,
            models,
            receivers,
        }
    }

    fn world_mut(&mut self) -> &mut World {
        &mut self.shard.logic_mut().world
    }

    fn step(&mut self, context: &str) {
        let report = self.shard.reconcile();
        assert!(
            report.refused.is_none(),
            "{context}: render refused: {:?}",
            report.refused
        );
        assert!(
            report.closed.is_empty(),
            "{context}: connections closed: {:?}",
            report.closed
        );

        for (connection, receiver) in &mut self.receivers {
            let model = self
                .models
                .get_mut(connection)
                .expect("one strict model per connection");
            while let Ok(message) = receiver.try_recv() {
                model.apply(&message);
            }
        }
        self.check_views(context);
        self.check_audio_visibility(context);
    }

    fn check_views(&self, context: &str) {
        for (connection, model) in &self.models {
            let attached = self.shard.connection(*connection).expect("still attached");
            let composed = self
                .shard
                .view()
                .restrict(attached.observation())
                .compose(attached.overlay_sent());

            let expected_channels: BTreeMap<u32, (Option<u32>, String)> = composed
                .channels
                .values()
                .map(|channel| {
                    (
                        channel.id.0,
                        (
                            (channel.id.0 != 0).then_some(channel.parent.0),
                            channel.name.clone(),
                        ),
                    )
                })
                .collect();
            let actual_channels: BTreeMap<u32, (Option<u32>, String)> = model
                .channels
                .iter()
                .map(|(id, channel)| (*id, (channel.parent, channel.name.clone())))
                .collect();
            assert_eq!(
                actual_channels, expected_channels,
                "{context}: channel view diverged for {connection:?}"
            );

            let expected_users: BTreeMap<u32, (String, u32)> = composed
                .users
                .values()
                .map(|user| (user.session.0, (user.name.clone(), user.channel.0)))
                .collect();
            let actual_users: BTreeMap<u32, (String, u32)> = model
                .users
                .iter()
                .map(|(session, user)| (*session, (user.name.clone(), user.channel)))
                .collect();
            assert_eq!(
                actual_users, expected_users,
                "{context}: user view diverged for {connection:?}"
            );
        }
    }

    fn check_audio_visibility(&self, context: &str) {
        let routing = self.shard.routing();
        let routing = routing.borrow();
        for sender in routing.senders() {
            for receiver in routing.receivers(sender) {
                let (_, model) = self
                    .models
                    .iter()
                    .find(|(connection, _)| {
                        self.shard
                            .connection(**connection)
                            .is_some_and(|attached| attached.session() == *receiver)
                    })
                    .expect("every routed receiver is attached");
                assert!(
                    model.users.contains_key(&sender.0),
                    "{context}: receiver {receiver:?} can hear hidden sender {sender:?}"
                );
            }
        }
    }
}

#[test]
fn scoped_publications_match_the_strict_client_across_all_change_classes() {
    const SEEDS: u64 = 400;
    const STEPS: usize = 24;

    for seed in 1..=SEEDS {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let players = 3 + rng.below(4);
        let mut harness = Harness::new(players);
        harness.step(&format!("seed {seed}: initial"));
        let mut classes_used = [false; 4];

        for step in 0..STEPS {
            let victim = rng.below(players);
            match rng.below(4) {
                0 => {
                    harness.world_mut().label = harness.world_mut().label.saturating_add(1);
                    classes_used[0] = true;
                }
                1 | 2 => {
                    let team = rng.below(TEAMS as usize) as u32;
                    if let Some(player) = harness.world_mut().players.get_mut(victim)
                        && player.team != team
                    {
                        player.team = team;
                        classes_used[1] = true;
                        classes_used[2] = true;
                    }
                }
                _ => {
                    if let Some(player) = harness.world_mut().players.get_mut(victim) {
                        player.vanished = !player.vanished;
                        classes_used[3] = true;
                    }
                }
            }
            harness.step(&format!("seed {seed}, step {step}"));
        }

        assert!(
            classes_used.into_iter().all(|used| used),
            "seed {seed} did not exercise every change class"
        );
    }
}

#[test]
fn every_compiled_audio_edge_names_a_visible_sender() {
    let mut harness = Harness::new(6);
    harness.step("initial");
    for index in 0..6 {
        if let Some(player) = harness.world_mut().players.get_mut(index) {
            player.vanished = index % 2 == 0;
        }
    }
    harness.step("half vanished");
}
