//! Transition planning (spec 12.5, 12.6) and the output transaction (12.7).
//!
//! [`plan`] turns a diff into an ordered sequence of [`PlanOp`]s that a client
//! can apply without ever observing a protocol violation. The order combines
//! the two ordered lists of spec 12.5 (addition/move and restriction/removal)
//! into one globally safe sequence, and it enforces the transition-safety rule
//! of spec 12.6:
//!
//! - a newly forbidden audio route is disabled **first**, before any view
//!   change (spec 20 invariant 18): audio-off precedes view for a prohibition;
//! - a newly allowed audio route is enabled **last**, after the view is prepared
//!   (invariant 19): view precedes audio-on for an authorization.
//!
//! Between those bookends the view mutates in an order that keeps every
//! intermediate state structurally valid: parents are created before children
//! (invariant 9), users are moved to their final channels before any channel is
//! removed so no occupied channel is deleted (invariant 8), and channels are
//! removed children-before-parents (invariant 10).
//!
//! The plan is abstract: a [`PlanOp`] is a view mutation, not a Mumble message.
//! Emitting `ChannelState`/`UserState`/`ChannelRemove`/... from these ops is the
//! session layer's job (Phase 3). The committed view is advanced to
//! [`OutputTransaction::next_view`] only once the whole transaction is accepted
//! by the output queue (spec 12.7, invariant 20); [`plan`] never mutates its
//! inputs.

use std::collections::{BTreeSet, HashSet};

use voxloom_render::{
    ActionKey, ChannelId, ClientView, ContextActionView, ListenerRelation, SessionId, ViewChannel,
    ViewUser,
};

use crate::diff::{ChannelPatch, PermissionUpdate, UserPatch, diff};

/// A directional audio route sender -> receiver (spec 6 "Audio route"). Audio
/// routing itself is a Phase 4 concern; the planner only needs the two route
/// sets so it can order route toggles around the visual transition (spec 12.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AudioRoute {
    pub sender: SessionId,
    pub receiver: SessionId,
}

/// One ordered step of a transition. Variants are documented with the spec 12.5
/// step they realize; [`plan`] emits them in the safe global order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanOp {
    /// Restriction step 1: stop a now-forbidden flow before any view change.
    DisableAudioRoute(AudioRoute),
    /// Addition steps 1-2: create a channel (parents emitted before children).
    CreateChannel(ViewChannel),
    /// Addition step 3: update a channel's properties.
    UpdateChannel(ChannelPatch),
    /// Addition step 4: add a new user, already in its channel.
    AddUser(ViewUser),
    /// Addition step 5: move an existing user to another (already existing) channel.
    MoveUser {
        session: SessionId,
        channel: ChannelId,
    },
    /// Addition step 6: update an existing user's non-channel state.
    UpdateUser(UserPatch),
    /// Restriction step 4: drop a listener relation.
    RemoveListener(ListenerRelation),
    /// View prep for a listen authorization: add a listener relation.
    AddListener(ListenerRelation),
    /// Restriction step 3: remove a user (vacating its channel).
    RemoveUser(SessionId),
    /// Restriction step 5: remove a channel (children emitted before parents).
    RemoveChannel(ChannelId),
    /// Addition step 7: publish an effective permission mask.
    UpdatePermissions(PermissionUpdate),
    /// Addition step 7: publish a context action.
    AddAction(ContextActionView),
    /// Restriction step 6: drop an obsolete context action.
    RemoveAction(ActionKey),
    /// Addition step 8: enable a newly allowed flow, after the view is ready.
    EnableAudioRoute(AudioRoute),
}

/// A planned transition (spec 12.7 `struct OutputTransaction`).
///
/// `next_view` is the view the connection will hold once every op is accepted by
/// the output queue. The caller commits it only on atomic acceptance (invariant
/// 20); until then the committed view is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputTransaction {
    pub from_revision: u64,
    pub to_revision: u64,
    pub ops: Vec<PlanOp>,
    pub next_view: ClientView,
}

/// Plan the transition from `committed` to `desired` (spec 12.5/12.6).
///
/// `committed_routes`/`desired_routes` are the connection's audio routes before
/// and after the transition; their difference drives the audio-off/on ordering
/// of spec 12.6. Both views are assumed normalized and valid (the pipeline runs
/// normalize + validate before plan, spec 12.2).
#[must_use]
pub fn plan(
    committed: &ClientView,
    desired: &ClientView,
    committed_routes: &BTreeSet<AudioRoute>,
    desired_routes: &BTreeSet<AudioRoute>,
    from_revision: u64,
    to_revision: u64,
) -> OutputTransaction {
    let delta = diff(committed, desired);
    let mut ops: Vec<PlanOp> = Vec::new();

    // Restriction, step 1: disable now-forbidden routes before touching the view.
    for route in committed_routes.difference(desired_routes) {
        ops.push(PlanOp::DisableAudioRoute(*route));
    }

    // Addition, steps 1-2: create channels, parents before children, so later
    // moves and property updates have their targets present.
    for channel in order_creations(&delta.channels_added, committed) {
        ops.push(PlanOp::CreateChannel(channel));
    }

    // Addition, step 3: channel property updates.
    for patch in &delta.channels_updated {
        ops.push(PlanOp::UpdateChannel(patch.clone()));
    }

    // Addition, step 4: new users, already placed in their channels.
    for user in &delta.users_added {
        ops.push(PlanOp::AddUser(user.clone()));
    }

    // Addition, step 5: move existing users to their final channels. Doing this
    // before any channel removal is what keeps invariant 8 (no occupied channel
    // deleted): after this pass no surviving user references a removed channel.
    for patch in &delta.users_updated {
        if let Some(channel) = patch.channel {
            ops.push(PlanOp::MoveUser {
                session: patch.session,
                channel,
            });
        }
    }

    // Addition, step 6: remaining user state, with the move stripped out.
    for patch in &delta.users_updated {
        let state_only = without_channel(patch);
        if !state_only.is_noop() {
            ops.push(PlanOp::UpdateUser(state_only));
        }
    }

    // Restriction step 4 then listen authorization: drop stale listeners, then
    // add new ones. Both happen before route enable so a listen view is ready
    // before its audio (spec 12.6).
    for update in &delta.listener_updates {
        if !update.active {
            ops.push(PlanOp::RemoveListener(update.relation));
        }
    }
    for update in &delta.listener_updates {
        if update.active {
            ops.push(PlanOp::AddListener(update.relation));
        }
    }

    // Restriction step 3 (removals): remove departed users, vacating channels.
    for session in &delta.users_removed {
        ops.push(PlanOp::RemoveUser(*session));
    }

    // Restriction step 5: remove channels, children before parents.
    for channel in order_removals(&delta.channels_removed, committed) {
        ops.push(PlanOp::RemoveChannel(channel));
    }

    // Addition step 7: publish permissions and actions; restriction step 6:
    // drop obsolete actions.
    for update in &delta.permissions_updated {
        ops.push(PlanOp::UpdatePermissions(*update));
    }
    for action in &delta.actions_added {
        ops.push(PlanOp::AddAction(action.clone()));
    }
    for key in &delta.actions_removed {
        ops.push(PlanOp::RemoveAction(key.clone()));
    }

    // Addition step 8: enable newly allowed routes, last, once the view is ready.
    for route in desired_routes.difference(committed_routes) {
        ops.push(PlanOp::EnableAudioRoute(*route));
    }

    OutputTransaction {
        from_revision,
        to_revision,
        ops,
        next_view: desired.clone(),
    }
}

/// A copy of `patch` with the channel move removed, for the step-6 state update.
fn without_channel(patch: &UserPatch) -> UserPatch {
    let mut state_only = patch.clone();
    state_only.channel = None;
    state_only
}

/// Order added channels so a parent is always emitted before its children
/// (invariant 9). A parent is either an already-present committed channel or a
/// channel created earlier in this pass; a valid desired tree guarantees this
/// terminates.
fn order_creations(added: &[ViewChannel], committed: &ClientView) -> Vec<ViewChannel> {
    let mut present: HashSet<ChannelId> = committed.channels.keys().copied().collect();
    let mut remaining: Vec<&ViewChannel> = added.iter().collect();
    let mut ordered: Vec<ViewChannel> = Vec::with_capacity(added.len());

    while !remaining.is_empty() {
        let mut progressed = false;
        let mut still_waiting: Vec<&ViewChannel> = Vec::new();
        for channel in remaining {
            // A channel that is its own parent (a new root) or whose parent is
            // already present can be created now.
            if channel.parent == channel.id || present.contains(&channel.parent) {
                present.insert(channel.id);
                ordered.push(channel.clone());
                progressed = true;
            } else {
                still_waiting.push(channel);
            }
        }
        remaining = still_waiting;
        if !progressed {
            // Defensive: a valid desired tree never reaches here. Emit the rest
            // in input order rather than loop forever (fail open on ordering is
            // safe: the caller validated the view; this only affects op order).
            ordered.extend(remaining.iter().map(|channel| (*channel).clone()));
            break;
        }
    }
    ordered
}

/// Order removed channels so a child is always emitted before its parent
/// (invariant 10), using the committed tree's parent relation. Deeper channels
/// come first; ties break by id for determinism.
fn order_removals(removed: &[ChannelId], committed: &ClientView) -> Vec<ChannelId> {
    let mut ordered: Vec<ChannelId> = removed.to_vec();
    ordered.sort_by(|left, right| {
        depth_in(committed, *right)
            .cmp(&depth_in(committed, *left))
            .then(left.cmp(right))
    });
    ordered
}

/// Depth of `channel` in the committed tree (root is depth 0). A broken chain or
/// a cycle stops the walk; this is only a sort key, and the input tree was
/// validated (spec 12.2), so neither occurs for real input.
fn depth_in(committed: &ClientView, channel: ChannelId) -> u32 {
    let mut depth = 0u32;
    let mut current = channel;
    let mut guard: HashSet<ChannelId> = HashSet::new();
    while current != committed.root_channel && guard.insert(current) {
        match committed.channels.get(&current) {
            Some(view_channel) => {
                current = view_channel.parent;
                depth = depth.saturating_add(1);
            }
            None => break,
        }
    }
    depth
}
