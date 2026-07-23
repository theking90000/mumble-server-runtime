//! Property and ordering tests for the transition planner (spec 12.5/12.6).
//!
//! The central property (roadmap Phase 5 "done"): for any two valid views
//! `(committed, desired)`, applying the plan to `committed` yields exactly
//! `desired` after normalization, and no intermediate state violates a
//! section-20 invariant.
//!
//! The shared testkit generators and the strict `SimulatedMumbleClient` (spec
//! 26.6) are Phase-3 verifier deliverables (R2) that do not exist yet, so this
//! file carries a self-contained deterministic generator and a pure view
//! `apply` that stands in for the client's message-application model. When P3
//! lands, the authoritative high-volume proptest should move to the testkit and
//! run against that client; the properties asserted here are the same ones it
//! will check.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};

use voxloom_reconcile::{AudioRoute, PlanOp, plan};
use voxloom_render::{
    ActionKey, ActionTarget, BlobRef, ChannelId, ChannelKey, ClientView, ContextActionView,
    ListenerRelation, PermissionBits, SemanticKey, SessionId, UserKey, ViewChannel, ViewUser,
    normalize, validate,
};

// ---------------------------------------------------------------------------
// Deterministic PRNG (xorshift64). Seeded, no external dependency; the seed is
// printed on failure so any counterexample is reproducible.
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        // Avoid the zero state, which xorshift cannot leave.
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
            (self.next() % u64::from(bound)) as u32
        }
    }

    fn chance(&mut self, num: u32, den: u32) -> bool {
        self.below(den) < num
    }

    fn maybe_blob(&mut self, tag: &str) -> Option<BlobRef> {
        if self.chance(1, 3) {
            Some(BlobRef(format!("{tag}-{}", self.below(4))))
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Generator: a shared universe of (id, key) channels and (session, key) users,
// from which committed and desired each select a subset with randomized
// properties. The id<->key binding is a bijection reused by both views, which
// is exactly the ADR-007 stability guarantee: a given id never changes key
// across renders for a connection.
// ---------------------------------------------------------------------------

const SELF_SESSION: u32 = 1;

fn gen_view(rng: &mut Rng, extra_channels: u32, users: u32) -> ClientView {
    // Root is always present; its properties may vary between views (a root
    // update is valid), its id/parent/key do not.
    let mut root = randomized_channel(rng, 0, 0, "root");
    root.parent = ChannelId(0);
    let mut channels: BTreeMap<ChannelId, ViewChannel> = BTreeMap::new();
    channels.insert(ChannelId(0), root);
    let mut included: Vec<ChannelId> = vec![ChannelId(0)];

    for id in 1..=extra_channels {
        if rng.chance(2, 3) {
            // Parent is any already-included channel; since parents always have a
            // smaller id than their child here, the tree is acyclic in every
            // view and no reparenting can create a transient cycle.
            let parent = included[rng.below(included.len() as u32) as usize];
            let mut channel = randomized_channel(rng, id, parent.0, &format!("ch{id}"));
            // Links point only at already-included channels (invariant 15).
            for candidate in &included {
                if rng.chance(1, 5) {
                    channel.links.insert(*candidate);
                }
            }
            channels.insert(ChannelId(id), channel);
            included.push(ChannelId(id));
        }
    }

    let mut view_users: BTreeMap<SessionId, ViewUser> = BTreeMap::new();
    for session in 1..=users {
        let force = session == SELF_SESSION;
        if force || rng.chance(3, 4) {
            let channel = included[rng.below(included.len() as u32) as usize];
            view_users.insert(SessionId(session), randomized_user(rng, session, channel));
        }
    }

    // Effective permissions for a subset of visible channels (non-empty masks;
    // normalize would drop empty ones anyway).
    let mut permissions: BTreeMap<ChannelId, PermissionBits> = BTreeMap::new();
    for id in &included {
        if rng.chance(1, 2) {
            let bits = (rng.next() as u32) & 0x1_FFFF; // 17 defined flags
            if bits != 0 {
                permissions.insert(*id, PermissionBits(bits));
            }
        }
    }

    // A few listeners referencing visible users and channels.
    let mut listeners: BTreeSet<ListenerRelation> = BTreeSet::new();
    let user_sessions: Vec<SessionId> = view_users.keys().copied().collect();
    if !user_sessions.is_empty() {
        for _ in 0..rng.below(3) {
            let user = user_sessions[rng.below(user_sessions.len() as u32) as usize];
            let channel = included[rng.below(included.len() as u32) as usize];
            listeners.insert(ListenerRelation { user, channel });
        }
    }

    // Context actions drawn from a small fixed key set, with varying presence.
    let mut context_actions: BTreeMap<ActionKey, ContextActionView> = BTreeMap::new();
    for name in ["invite", "kick", "info"] {
        if rng.chance(1, 2) {
            let key = ActionKey(SemanticKey::Static(name.to_owned()));
            context_actions.insert(
                key.clone(),
                ContextActionView {
                    key,
                    target: match rng.below(3) {
                        0 => ActionTarget::Server,
                        1 => ActionTarget::Channel,
                        _ => ActionTarget::User,
                    },
                    label: format!("{name}-{}", rng.below(3)),
                },
            );
        }
    }

    ClientView {
        root_channel: ChannelId(0),
        channels,
        users: view_users,
        listeners,
        permissions,
        context_actions,
        // Server presentation is a connection constant for a transition (spec
        // 12.4 has no presentation delta); keep it identical across the pair.
        server_presentation: Default::default(),
    }
}

fn randomized_channel(rng: &mut Rng, id: u32, parent: u32, base_name: &str) -> ViewChannel {
    ViewChannel {
        key: ChannelKey(SemanticKey::Static(format!("k{id}"))),
        id: ChannelId(id),
        parent: ChannelId(parent),
        name: format!("{base_name}-{}", rng.below(3)),
        description: rng.maybe_blob("desc"),
        position: rng.below(5) as i32 - 2,
        temporary: rng.chance(1, 4),
        max_users: if rng.chance(1, 3) {
            Some(rng.below(20))
        } else {
            None
        },
        enter_restricted: rng.chance(1, 3),
        can_enter: rng.chance(2, 3),
        links: BTreeSet::new(),
    }
}

fn randomized_user(rng: &mut Rng, session: u32, channel: ChannelId) -> ViewUser {
    ViewUser {
        key: UserKey(SemanticKey::Static(format!("us{session}"))),
        session: SessionId(session),
        name: format!("user{session}-{}", rng.below(3)),
        channel,
        user_id: if rng.chance(1, 2) {
            Some(rng.below(100))
        } else {
            None
        },
        certificate_hash: if rng.chance(1, 2) {
            Some(format!("cert{}", rng.below(3)))
        } else {
            None
        },
        mute: rng.chance(1, 4),
        deaf: rng.chance(1, 5),
        suppress: rng.chance(1, 5),
        self_mute: rng.chance(1, 4),
        self_deaf: rng.chance(1, 5),
        priority_speaker: rng.chance(1, 6),
        recording: rng.chance(1, 6),
        comment: rng.maybe_blob("comment"),
        texture: rng.maybe_blob("texture"),
    }
}

fn gen_routes(rng: &mut Rng, view: &ClientView) -> BTreeSet<AudioRoute> {
    let sessions: Vec<SessionId> = view.users.keys().copied().collect();
    let mut routes = BTreeSet::new();
    if sessions.len() < 2 {
        return routes;
    }
    for _ in 0..rng.below(4) {
        let sender = sessions[rng.below(sessions.len() as u32) as usize];
        let receiver = sessions[rng.below(sessions.len() as u32) as usize];
        if sender != receiver {
            routes.insert(AudioRoute { sender, receiver });
        }
    }
    routes
}

// ---------------------------------------------------------------------------
// Pure view applier: stands in for the client's message-application model.
// Audio-route ops have no effect on the view (they belong to the audio plane).
// ---------------------------------------------------------------------------

fn apply(committed: &ClientView, ops: &[PlanOp]) -> ClientView {
    let mut view = committed.clone();
    for op in ops {
        apply_op(&mut view, op);
    }
    view
}

fn apply_op(view: &mut ClientView, op: &PlanOp) {
    match op {
        PlanOp::DisableAudioRoute(_) | PlanOp::EnableAudioRoute(_) => {}
        PlanOp::CreateChannel(channel) => {
            view.channels.insert(channel.id, channel.clone());
        }
        PlanOp::UpdateChannel(patch) => {
            if let Some(channel) = view.channels.get_mut(&patch.id) {
                if let Some(parent) = patch.parent {
                    channel.parent = parent;
                }
                if let Some(name) = &patch.name {
                    channel.name = name.clone();
                }
                if let Some(description) = &patch.description {
                    channel.description = description.clone();
                }
                if let Some(position) = patch.position {
                    channel.position = position;
                }
                if let Some(temporary) = patch.temporary {
                    channel.temporary = temporary;
                }
                if let Some(max_users) = patch.max_users {
                    channel.max_users = max_users;
                }
                if let Some(enter_restricted) = patch.enter_restricted {
                    channel.enter_restricted = enter_restricted;
                }
                if let Some(can_enter) = patch.can_enter {
                    channel.can_enter = can_enter;
                }
                if let Some(links) = &patch.links {
                    channel.links = links.clone();
                }
            }
        }
        PlanOp::AddUser(user) => {
            view.users.insert(user.session, user.clone());
        }
        PlanOp::MoveUser { session, channel } => {
            if let Some(user) = view.users.get_mut(session) {
                user.channel = *channel;
            }
        }
        PlanOp::UpdateUser(patch) => {
            if let Some(user) = view.users.get_mut(&patch.session) {
                if let Some(name) = &patch.name {
                    user.name = name.clone();
                }
                if let Some(user_id) = patch.user_id {
                    user.user_id = user_id;
                }
                if let Some(certificate_hash) = &patch.certificate_hash {
                    user.certificate_hash = certificate_hash.clone();
                }
                if let Some(mute) = patch.mute {
                    user.mute = mute;
                }
                if let Some(deaf) = patch.deaf {
                    user.deaf = deaf;
                }
                if let Some(suppress) = patch.suppress {
                    user.suppress = suppress;
                }
                if let Some(self_mute) = patch.self_mute {
                    user.self_mute = self_mute;
                }
                if let Some(self_deaf) = patch.self_deaf {
                    user.self_deaf = self_deaf;
                }
                if let Some(priority_speaker) = patch.priority_speaker {
                    user.priority_speaker = priority_speaker;
                }
                if let Some(recording) = patch.recording {
                    user.recording = recording;
                }
                if let Some(comment) = &patch.comment {
                    user.comment = comment.clone();
                }
                if let Some(texture) = &patch.texture {
                    user.texture = texture.clone();
                }
            }
        }
        PlanOp::RemoveListener(relation) => {
            view.listeners.remove(relation);
        }
        PlanOp::AddListener(relation) => {
            view.listeners.insert(*relation);
        }
        PlanOp::RemoveUser(session) => {
            view.users.remove(session);
        }
        PlanOp::RemoveChannel(id) => {
            view.channels.remove(id);
            // The client forgets a removed channel's associated state: its
            // permission entry (no other op clears it) and any link that other
            // channels held to it. Modelling this cleanup is what keeps a
            // removal of one link-referencing channel from leaving a transient
            // dangling link while a sibling that also links to it is still
            // present (invariant 15 holds at every step).
            view.permissions.remove(id);
            for channel in view.channels.values_mut() {
                channel.links.remove(id);
            }
        }
        PlanOp::UpdatePermissions(update) => {
            view.permissions.insert(update.channel, update.permissions);
        }
        PlanOp::AddAction(action) => {
            view.context_actions
                .insert(action.key.clone(), action.clone());
        }
        PlanOp::RemoveAction(key) => {
            view.context_actions.remove(key);
        }
    }
}

fn is_view_op(op: &PlanOp) -> bool {
    !matches!(
        op,
        PlanOp::DisableAudioRoute(_) | PlanOp::EnableAudioRoute(_)
    )
}

// ---------------------------------------------------------------------------
// Properties.
// ---------------------------------------------------------------------------

const SEEDS: u64 = 4000;

/// Generate a valid `(committed, desired)` pair and their route sets from a seed.
fn scenario(
    seed: u64,
) -> (
    ClientView,
    ClientView,
    BTreeSet<AudioRoute>,
    BTreeSet<AudioRoute>,
) {
    let mut rng = Rng::new(seed);
    let channels = rng.below(6);
    let users = 1 + rng.below(5);
    let committed = normalize(&gen_view(&mut rng, channels, users));
    let desired = normalize(&gen_view(&mut rng, channels, users));
    let committed_routes = gen_routes(&mut rng, &committed);
    let desired_routes = gen_routes(&mut rng, &desired);
    (committed, desired, committed_routes, desired_routes)
}

#[test]
fn generated_views_are_valid() {
    let self_session = Some(SessionId(SELF_SESSION));
    for seed in 0..SEEDS {
        let (committed, desired, _, _) = scenario(seed);
        validate(&committed, self_session)
            .unwrap_or_else(|error| panic!("seed {seed}: committed invalid: {error}"));
        validate(&desired, self_session)
            .unwrap_or_else(|error| panic!("seed {seed}: desired invalid: {error}"));
    }
}

#[test]
fn plan_applied_to_committed_yields_desired() {
    for seed in 0..SEEDS {
        let (committed, desired, committed_routes, desired_routes) = scenario(seed);
        let transaction = plan(
            &committed,
            &desired,
            &committed_routes,
            &desired_routes,
            1,
            2,
        );

        let reached = normalize(&apply(&committed, &transaction.ops));
        assert_eq!(
            reached, desired,
            "seed {seed}: plan did not reconstruct the desired view"
        );
        // Invariant 20: the transaction carries the next view; committed is
        // untouched, and next_view is exactly the desired (normalized) view.
        assert_eq!(
            transaction.next_view, desired,
            "seed {seed}: next_view mismatch"
        );
        assert_eq!(transaction.from_revision, 1);
        assert_eq!(transaction.to_revision, 2);
    }
}

#[test]
fn no_intermediate_state_violates_an_invariant() {
    let self_session = Some(SessionId(SELF_SESSION));
    for seed in 0..SEEDS {
        let (committed, desired, committed_routes, desired_routes) = scenario(seed);
        let transaction = plan(
            &committed,
            &desired,
            &committed_routes,
            &desired_routes,
            1,
            2,
        );

        let mut current = committed.clone();
        for (step, op) in transaction.ops.iter().enumerate() {
            // Ordering guards (spec 12.5): parents before children on create,
            // children before parents on remove, no occupied channel removed.
            match op {
                PlanOp::CreateChannel(channel) => {
                    assert!(
                        channel.parent == channel.id
                            || current.channels.contains_key(&channel.parent),
                        "seed {seed} step {step}: created child before its parent (inv 9)"
                    );
                }
                PlanOp::RemoveChannel(id) => {
                    assert!(
                        !current.users.values().any(|user| user.channel == *id),
                        "seed {seed} step {step}: removed an occupied channel (inv 8)"
                    );
                    assert!(
                        !current
                            .channels
                            .values()
                            .any(|channel| channel.id != *id && channel.parent == *id),
                        "seed {seed} step {step}: removed a parent before its children (inv 10)"
                    );
                }
                _ => {}
            }
            apply_op(&mut current, op);
            validate(&current, self_session).unwrap_or_else(|error| {
                panic!("seed {seed} step {step}: intermediate state invalid: {error}")
            });
        }
    }
}

#[test]
fn security_ordering_audio_off_before_view_before_audio_on() {
    for seed in 0..SEEDS {
        let (committed, desired, committed_routes, desired_routes) = scenario(seed);
        let transaction = plan(
            &committed,
            &desired,
            &committed_routes,
            &desired_routes,
            1,
            2,
        );
        let ops = &transaction.ops;

        let last_disable = ops
            .iter()
            .rposition(|op| matches!(op, PlanOp::DisableAudioRoute(_)));
        let first_enable = ops
            .iter()
            .position(|op| matches!(op, PlanOp::EnableAudioRoute(_)));
        let first_view = ops.iter().position(is_view_op);
        let last_view = ops.iter().rposition(is_view_op);

        // Invariant 18: every forbidden route is disabled before any view change.
        if let (Some(disable), Some(view)) = (last_disable, first_view) {
            assert!(
                disable < view,
                "seed {seed}: a view op precedes an audio-off op (inv 18)"
            );
        }
        // Invariant 19: every newly allowed route is enabled after every view op.
        if let (Some(enable), Some(view)) = (first_enable, last_view) {
            assert!(
                enable > view,
                "seed {seed}: an audio-on op precedes a view op (inv 19)"
            );
        }
    }
}

#[test]
fn empty_diff_produces_no_view_ops() {
    // Same view on both sides, same routes: nothing to do.
    let (committed, _, routes, _) = scenario(7);
    let transaction = plan(&committed, &committed, &routes, &routes, 5, 5);
    assert!(
        transaction.ops.is_empty(),
        "an identity transition emits no operations"
    );
    assert_eq!(transaction.next_view, committed);
}
