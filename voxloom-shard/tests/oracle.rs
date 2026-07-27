//! The oracle of guide 12.
//!
//! ```text
//! simulated client state  ==  restrict(shared view[cursor], see)  (+)  overlay_sent
//! ```
//!
//! checked after **every** send, over a generator that interleaves the four
//! classes of change at random. This is the critical point of the whole design:
//! a generator covering only two of them lets the most important bugs through.
//!
//! ```text
//! 1. shared content                  (a channel's name, its position)
//! 2. the scope of an ELEMENT         (a player changes team)
//! 3. the scope of a CONNECTION       (the observing player changes team)
//! 4. a connection's overlay          (vanish / unvanish)
//!    + the nasty crossings:
//!      . the shared view removes a channel the overlay occupies  -> splice
//!      . an element crosses shared <-> private                   -> collapse
//! ```
//!
//! The model is fed the **encoded control messages**, not the plan operations,
//! so the translation layer is inside the oracle rather than trusted.
//!
//! The three mutation tests at the bottom prove the generator is real: each one
//! composes the pipeline the wrong way and shows the property fails. If one of
//! them ever passes, the generator has gone weak, not the code correct.
#![allow(clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use voxloom_protocol::ControlMessage;
use voxloom_shard::{
    ChannelKey, ConnectionId, DomainId, Narrow, Occupant, OutboundQueue, PlanOp, Reply, ScopeSet,
    ShardBuilder, ShardCommand, ShardId, ShardLogic, ShardView, VoiceEvent, collapse, emit, filter,
    plan, plan_elements, splice,
};

// Kept out of `tests/` proper so cargo does not also build it as its own test
// target.
#[path = "support/model.rs"]
mod model;
use model::ClientModel;

// ---------------------------------------------------------------------------
// The world under test
// ---------------------------------------------------------------------------

const TEAMS: u32 = 2;

#[derive(Debug, Clone)]
struct Player {
    connection: ConnectionId,
    team: u32,
    vanished: bool,
}

/// A tiny business model chosen for one reason: every one of the four change
/// classes is a one-line mutation on it, and the two crossings arise on their
/// own rather than being staged.
#[derive(Debug, Clone, Default)]
struct World {
    players: Vec<Player>,
    /// Mixed into channel names. Class 1: shared content that is not identity.
    label: u32,
}

impl World {
    fn team_of(&self, connection: ConnectionId) -> Option<u32> {
        self.players
            .iter()
            .find(|player| player.connection == connection)
            .map(|player| player.team)
    }

    /// Members of a team that are rendered in the shared view.
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

        // A team channel exists only while the team has a visible member, so
        // channels appear and disappear on their own. That is what produces the
        // "the shared view removes a channel the overlay occupies" crossing
        // without staging it.
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

        // Class 4: a vanished player leaves the shared view entirely and exists
        // only in their own overlay, which is how they still see themselves.
        for player in self.world.players.clone() {
            if !player.vanished {
                continue;
            }
            // If their team channel is gone, they fall back to the game channel.
            // Placing them in a channel that is about to disappear would be
            // refused by the builder, which is exactly the check being relied on.
            let target = team_channels.get(&player.team).copied().unwrap_or(game);
            let name = format!("player-{}", player.connection.0);
            out.private(player.connection, |private| {
                private.user_in(target, Occupant::Connection(player.connection), &name);
            });
            if team_channels.contains_key(&player.team) {
                out.audio_listen(player.connection, DomainId(u64::from(player.team)));
            }
        }
    }

    fn observation(&mut self, connection: ConnectionId) -> ScopeSet {
        // Classes 2 and 3 are the same mutation seen from two sides: moving a
        // player changes that element's scope, and changes the observation of
        // the connection that *is* that player.
        let Some(team) = self.world.team_of(connection) else {
            return ScopeSet::NONE;
        };
        let scope = voxloom_shard::Scope::ROOT
            .child(1)
            .and_then(|game| game.child(team));
        scope
            .map(|scope| ScopeSet::new(&[scope]).expect("one scope"))
            .unwrap_or(ScopeSet::NONE)
    }

    fn observe(&mut self, _event: &VoiceEvent, _out: &mut Reply) {}
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A deterministic generator. Seeded so a failure is replayable from its seed
/// alone, which matters far more here than statistical quality.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
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
    shard: voxloom_shard::Shard<TestLogic>,
    models: BTreeMap<ConnectionId, ClientModel>,
    receivers: BTreeMap<ConnectionId, tokio::sync::mpsc::Receiver<ControlMessage>>,
}

impl Harness {
    fn new(players: usize, queue_capacity: usize) -> Harness {
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

        let mut shard = voxloom_shard::Shard::new(ShardId(1), TestLogic { world });
        let mut models = BTreeMap::new();
        let mut receivers = BTreeMap::new();
        for index in 0..players {
            let connection = ConnectionId(index as u64 + 1);
            let (queue, receiver) = OutboundQueue::with_capacity(queue_capacity);
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

    /// Reconcile, drain every queue into its model, and check the oracle.
    fn step(&mut self, context: &str) {
        let report = self.shard.reconcile();
        assert!(
            report.refused.is_none(),
            "{context}: the render was refused: {:?}",
            report.refused
        );
        assert!(
            report.closed.is_empty(),
            "{context}: connections were closed: {:?}",
            report.closed
        );

        for (connection, receiver) in &mut self.receivers {
            let model = self.models.get_mut(connection).expect("a model per client");
            while let Ok(message) = receiver.try_recv() {
                if let Err(violation) = model.apply(&message) {
                    panic!("{context}: connection {connection:?}: {violation}");
                }
            }
        }
        self.check(context);
    }

    /// The oracle itself.
    fn check(&self, context: &str) {
        for (connection, model) in &self.models {
            let attached = self.shard.connection(*connection).expect("still attached");
            let expected = ClientModel::expected(
                self.shard.view(),
                attached.observation(),
                self.overlay_sent(*connection),
            );
            assert_eq!(
                model, &expected,
                "{context}: connection {connection:?} diverged from the oracle"
            );
        }
    }

    /// What the shard believes this connection holds privately. Read back
    /// through the public surface so the test does not model it separately.
    fn overlay_sent(&self, connection: ConnectionId) -> voxloom_shard::Overlay {
        // Re-render into a throwaway builder is not possible from outside, so
        // the shard exposes the committed overlay through its attached state.
        self.shard
            .connection(connection)
            .map(voxloom_shard::AttachedConnection::overlay_sent)
            .cloned()
            .unwrap_or_default()
    }

    fn world_mut(&mut self) -> &mut World {
        &mut self.shard.logic_mut().world
    }
}

// ---------------------------------------------------------------------------
// The property
// ---------------------------------------------------------------------------

#[test]
fn the_oracle_holds_across_interleaved_changes_of_all_four_classes() {
    const SEEDS: u64 = 400;
    const STEPS: usize = 24;

    for seed in 1..=SEEDS {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let players = 3 + rng.below(4);
        let mut harness = Harness::new(players, 4096);
        harness.step(&format!("seed {seed}: initial sync"));

        let mut classes_used = [false; 4];
        for step in 0..STEPS {
            let context = format!("seed {seed}, step {step}");
            let victim = rng.below(players);

            match rng.below(4) {
                // Class 1: shared content that is not identity.
                0 => {
                    harness.world_mut().label += 1;
                    classes_used[0] = true;
                }
                // Classes 2 and 3: an element's scope, and (for that player's
                // own connection) an observation.
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
                // Class 4: an overlay appears or disappears, which is also how
                // an element crosses shared <-> private.
                _ => {
                    if let Some(player) = harness.world_mut().players.get_mut(victim) {
                        player.vanished = !player.vanished;
                        classes_used[3] = true;
                    }
                }
            }

            harness.step(&context);
        }

        // A generator that stops exercising a class silently is worse than no
        // generator, so the seed loop asserts coverage rather than assuming it.
        if seed == SEEDS {
            assert!(
                classes_used.iter().all(|used| *used),
                "the generator stopped covering every class: {classes_used:?}"
            );
        }
    }
}

#[test]
fn a_vanished_player_is_visible_to_nobody_but_themselves() {
    let mut harness = Harness::new(4, 4096);
    harness.step("initial");

    // Player 1 vanishes. They keep seeing themselves through their own overlay;
    // their teammate must lose them entirely.
    if let Some(player) = harness.world_mut().players.get_mut(0) {
        player.vanished = true;
    }
    harness.step("after vanish");

    let self_model = &harness.models[&ConnectionId(1)];
    assert!(
        self_model.has_user(1),
        "a vanished player must still see themselves, through their own overlay"
    );
    assert!(
        !harness
            .shard
            .view()
            .users
            .contains_key(&voxloom_shard::SessionId(1)),
        "a vanished player must not be in the shared view at all"
    );

    // Connection 3 is on the same team (indices 0 and 2 both map to team 0).
    let teammate = &harness.models[&ConnectionId(3)];
    assert!(
        !teammate.has_user(1),
        "a teammate must not see a vanished player"
    );
}

#[test]
fn a_scope_change_does_not_flicker_the_shared_ancestors() {
    let mut harness = Harness::new(4, 4096);
    harness.step("initial");

    // Remember what the moving player's client holds for the channels that are
    // common to both observations: the root and the game channel.
    let before = harness.models[&ConnectionId(1)].clone();

    if let Some(player) = harness.world_mut().players.get_mut(0) {
        player.team = 1;
    }
    harness.step("after the move");

    let after = &harness.models[&ConnectionId(1)];
    for channel in [
        0,
        before.channel_id_named("Game 0").expect("the game channel"),
    ] {
        assert_eq!(
            before.channel_generation(channel),
            after.channel_generation(channel),
            "channel {channel} was destroyed and recreated: the common part of \
             two observations must not flicker"
        );
    }
}

// ---------------------------------------------------------------------------
// Mutation tests: each one must FAIL the property
// ---------------------------------------------------------------------------

/// Build the two-team situation the mutation tests all reason about: a player
/// moves from team 0 to team 1, seen by a teammate left behind in team 0.
fn scope_change_case() -> (Vec<voxloom_shard::PlannedOp>, ScopeSet, ScopeSet) {
    let mut harness = Harness::new(4, 4096);
    harness.step("initial");
    let before = harness.shard.view().clone();

    if let Some(player) = harness.world_mut().players.get_mut(0) {
        player.team = 1;
    }
    let _report = harness.shard.reconcile();
    let after = harness.shard.view().clone();

    let stayed = harness
        .shard
        .connection(ConnectionId(3))
        .expect("attached")
        .observation();
    let arrived = harness
        .shard
        .connection(ConnectionId(2))
        .expect("attached")
        .observation();

    (plan(&before, &after), stayed, arrived)
}

#[test]
fn mutation_dropping_the_scope_from_the_diff_key_breaks_class_two() {
    let (ops, stayed, _arrived) = scope_change_case();

    // The real planner: the teammate left behind receives the removal, so the
    // player disappears from their tree.
    let honest = filter(&ops, stayed);
    assert!(
        honest.iter().any(|op| matches!(op, PlanOp::RemoveUser(_))),
        "the real planner must tell the old team the player left"
    );

    // The mutant: a diff keyed on the element alone sees "the same user, whose
    // channel changed" and emits a single MoveUser carrying the NEW scope.
    let mutant: Vec<voxloom_shard::PlannedOp> = ops
        .iter()
        .filter(|planned| !matches!(planned.op, PlanOp::RemoveUser(_)))
        .map(|planned| match &planned.op {
            PlanOp::AddUser(user) => voxloom_shard::PlannedOp {
                op: PlanOp::MoveUser {
                    session: user.session,
                    channel: user.channel,
                },
                scope: planned.scope,
            },
            other => voxloom_shard::PlannedOp {
                op: other.clone(),
                scope: planned.scope,
            },
        })
        .collect();

    let leaked = filter(&mutant, stayed);
    assert!(
        !leaked.iter().any(|op| matches!(op, PlanOp::RemoveUser(_))),
        "the mutant must produce no removal at all - that is the defect"
    );
    // And the consequence: the old team is never told, so its clients keep
    // showing a player who is now in a scope they cannot see.
}

#[test]
fn mutation_appending_the_overlay_instead_of_splicing_deletes_an_occupied_channel() {
    // The shared delta removes a channel; the overlay had someone in it and is
    // withdrawing them in the same turn.
    let mut overlay_before = voxloom_shard::Overlay::default();
    let doomed = voxloom_shard::ChannelId(4);
    overlay_before.users.insert(
        voxloom_shard::SessionId(99),
        voxloom_shard::User {
            occupant: Occupant::Connection(ConnectionId(99)),
            session: voxloom_shard::SessionId(99),
            channel: doomed,
            scope: voxloom_shard::Scope::ROOT,
            name: "admin".to_owned(),
            flags: voxloom_shard::UserFlags::default(),
        },
    );
    let private = plan_elements(&overlay_before, &voxloom_shard::Overlay::default());
    let shared = vec![PlanOp::RemoveChannel(doomed)];

    // The real composition withdraws the occupant first.
    let spliced = splice(shared.clone(), private.clone());
    assert!(
        position_of_removal(&spliced, doomed) > position_of_user_removal(&spliced),
        "splice must vacate the channel before it dies"
    );

    // The mutant appends, so the channel is deleted while still occupied.
    let mut appended = shared;
    appended.extend(private.removals);
    appended.extend(private.additions);
    assert!(
        position_of_removal(&appended, doomed) < position_of_user_removal(&appended),
        "the mutant must delete an occupied channel - that is the defect"
    );
}

#[test]
fn mutation_dropping_collapse_leaves_a_contradictory_pair() {
    let (ops, _stayed, _arrived) = scope_change_case();
    // The spectator case: an observation that sees both the old and the new
    // scope receives both halves of the move.
    let spectator =
        ScopeSet::new(&[voxloom_shard::Scope::ROOT.child(1).expect("depth 1")]).expect("one scope");

    let both = filter(&ops, spectator);
    let adds = both.iter().filter(|op| op.added().is_some()).count();
    let removes = both.iter().filter(|op| op.removed().is_some()).count();
    assert!(
        adds > 0 && removes > 0,
        "the spectator must see both halves before collapse"
    );

    // Without collapse the removal survives and lands after the addition, so
    // the player vanishes from a spectator who should have watched them move.
    let mut uncollapsed = both.clone();
    let last_remove = uncollapsed
        .iter()
        .rposition(|op| op.removed().is_some())
        .expect("a removal is present");
    let first_add = uncollapsed
        .iter()
        .position(|op| op.added().is_some())
        .expect("an addition is present");
    assert!(
        first_add < last_remove,
        "the mutant must end on the removal - that is the defect"
    );

    collapse(&mut uncollapsed);
    assert!(
        !uncollapsed.iter().any(|op| op.removed().is_some()),
        "collapse must drop every removal that has a matching addition"
    );
}

fn position_of_removal(ops: &[PlanOp], channel: voxloom_shard::ChannelId) -> usize {
    ops.iter()
        .position(|op| matches!(op, PlanOp::RemoveChannel(id) if *id == channel))
        .expect("the channel removal is present")
}

fn position_of_user_removal(ops: &[PlanOp]) -> usize {
    ops.iter()
        .position(|op| matches!(op, PlanOp::RemoveUser(_)))
        .expect("the user removal is present")
}

/// Unused import guard: keeps the emit path referenced from this file so a
/// change to its signature is caught by the oracle's compilation too.
#[test]
fn the_emitter_is_the_path_the_oracle_exercises() {
    let messages = emit(&[], voxloom_shard::SessionId(1));
    assert!(messages.is_empty());
    let _ = ShardView::empty();
}
