//! Planning a transition, with the scope as part of the element's identity.
//!
//! # The one idea in this module
//!
//! **The diff's comparison key is `(element, scope)`, not `element`.** It costs
//! nothing and it deletes a whole family of special cases.
//!
//! When a player changes team, their scope goes from `/g7/t3` to `/g7/t2`. That
//! is not "a field that changed", it is *the entry `(B, /g7/t3)` disappearing and
//! the entry `(B, /g7/t2)` appearing*. So the ordinary diff produces, on its own:
//!
//! ```text
//! RemoveUser(B)               [scope /g7/t3]
//! AddUser(B, channel Team2)   [scope /g7/t2]
//! ```
//!
//! and the per-connection filter stays a single branchless test:
//!
//! | connection | observes | receives |
//! |---|---|---|
//! | teammate still in t3 | `{/g7/t3}` | the `Remove` alone, so B leaves |
//! | player in t2 | `{/g7/t2}` | the `Add` alone, so B arrives |
//! | spectator | `{/g7}` | **both**, settled by [`crate::compose::collapse`] |
//! | player in another game | `{/g8}` | nothing |
//!
//! A property falls out for free: a move *within* a scope stays a `MoveUser`,
//! while a move *between* scopes becomes a departure and an arrival. Which is
//! semantically exactly right.
//!
//! The identifier does **not** change: it is the same person, and the client's
//! local preferences for them must survive the move. Only the diff's notion of
//! *sameness* carries the scope.
//!
//! # No audio operations
//!
//! Audio comes from a separate table whose ordering is guaranteed differently
//! (guide 9.5), so the plan is purely a sequence of view mutations.
//!
//! REF: docs/design/guide-implementation.md 5

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::ids::{ChannelId, SessionId};
use crate::scope::Scope;
use crate::view::{Channel, Overlay, ShardView, User, UserFlags};

/// A sparse channel change.
///
/// Links are carried as two explicit sets rather than as a replacement, because
/// the client treats a non-empty `links` list as a full replacement but ignores
/// an empty one entirely. Stating additions and removals separately makes the
/// patch composable across a replay and removes the need to know what the
/// connection currently holds.
///
/// REF: references/mumble/src/mumble/Messages.cpp : `msgChannelState` handles
///   `links_remove` and `links_add` in their own blocks, independent of `links`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelPatch {
    pub id: ChannelId,
    pub parent: Option<ChannelId>,
    pub name: Option<String>,
    pub position: Option<i32>,
    pub can_enter: Option<bool>,
    pub links_added: BTreeSet<ChannelId>,
    pub links_removed: BTreeSet<ChannelId>,
}

impl ChannelPatch {
    fn empty(id: ChannelId) -> ChannelPatch {
        ChannelPatch {
            id,
            parent: None,
            name: None,
            position: None,
            can_enter: None,
            links_added: BTreeSet::new(),
            links_removed: BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.parent.is_none()
            && self.name.is_none()
            && self.position.is_none()
            && self.can_enter.is_none()
            && self.links_added.is_empty()
            && self.links_removed.is_empty()
    }
}

/// A sparse user change. The channel move is a separate operation, because it
/// has to be ordered before any channel removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserPatch {
    pub session: SessionId,
    pub name: Option<String>,
    pub flags: Option<UserFlags>,
}

impl UserPatch {
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.name.is_none() && self.flags.is_none()
    }
}

/// One view mutation. Not a Mumble message: spelling these on the wire is
/// [`mod@crate::emit`]'s job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanOp {
    CreateChannel(Channel),
    UpdateChannel(ChannelPatch),
    AddUser(User),
    MoveUser {
        session: SessionId,
        channel: ChannelId,
    },
    UpdateUser(UserPatch),
    RemoveUser(SessionId),
    RemoveChannel(ChannelId),
}

/// What an operation identifies, for [`crate::compose::collapse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ElementId {
    Channel(ChannelId),
    User(SessionId),
}

impl PlanOp {
    /// The element this operation brings into existence, if any.
    #[must_use]
    pub fn added(&self) -> Option<ElementId> {
        match self {
            PlanOp::CreateChannel(channel) => Some(ElementId::Channel(channel.id)),
            PlanOp::AddUser(user) => Some(ElementId::User(user.session)),
            _ => None,
        }
    }

    /// The element this operation withdraws, if any.
    #[must_use]
    pub fn removed(&self) -> Option<ElementId> {
        match self {
            PlanOp::RemoveChannel(channel) => Some(ElementId::Channel(*channel)),
            PlanOp::RemoveUser(session) => Some(ElementId::User(*session)),
            _ => None,
        }
    }

    /// Whether this operation belongs to the removal phases (P6, P7).
    ///
    /// [`crate::compose::splice`] uses this to find where the overlay's
    /// operations have to be inserted.
    #[must_use]
    pub fn is_removal(&self) -> bool {
        matches!(self, PlanOp::RemoveUser(_) | PlanOp::RemoveChannel(_))
    }
}

/// An operation together with the scope that decides who receives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedOp {
    pub op: PlanOp,
    /// For a removal this is the scope in the **previous** view: the element no
    /// longer exists in the new one, so it cannot be read back from there.
    pub scope: Scope,
}

/// Plan the transition from `before` to `after`.
///
/// The global order carries the ordering invariants:
///
/// ```text
/// P1  CreateChannel      (parents before children)
/// P2  UpdateChannel
/// P3  AddUser
/// P4  MoveUser           (before any removal: no occupied channel is deleted)
/// P5  UpdateUser
/// P6  RemoveUser
/// P7  RemoveChannel      (children before parents)
/// ```
#[must_use]
pub fn plan(before: &ShardView, after: &ShardView) -> Vec<PlannedOp> {
    let mut creates: Vec<Channel> = Vec::new();
    let mut updates: Vec<PlannedOp> = Vec::new();
    let mut adds: Vec<PlannedOp> = Vec::new();
    let mut moves: Vec<PlannedOp> = Vec::new();
    let mut user_updates: Vec<PlannedOp> = Vec::new();
    let mut user_removals: Vec<PlannedOp> = Vec::new();
    let mut channel_removals: Vec<(ChannelId, Scope)> = Vec::new();

    for (id, new) in &after.channels {
        match before.channels.get(id) {
            // Same identity, same scope: an ordinary field-level update.
            Some(old) if old.scope == new.scope => {
                let patch = channel_patch(old, new);
                if !patch.is_noop() {
                    updates.push(PlannedOp {
                        op: PlanOp::UpdateChannel(patch),
                        scope: new.scope,
                    });
                }
            }
            // Same identity, different scope: a different entry in the diff, so
            // it departs from the old scope and arrives in the new one.
            Some(old) => {
                creates.push(new.clone());
                channel_removals.push((*id, old.scope));
            }
            None => creates.push(new.clone()),
        }
    }
    for (id, old) in &before.channels {
        if !after.channels.contains_key(id) {
            channel_removals.push((*id, old.scope));
        }
    }

    for (session, new) in &after.users {
        match before.users.get(session) {
            Some(old) if old.scope == new.scope => {
                if old.channel != new.channel {
                    moves.push(PlannedOp {
                        op: PlanOp::MoveUser {
                            session: *session,
                            channel: new.channel,
                        },
                        scope: new.scope,
                    });
                }
                let patch = user_patch(old, new);
                if !patch.is_noop() {
                    user_updates.push(PlannedOp {
                        op: PlanOp::UpdateUser(patch),
                        scope: new.scope,
                    });
                }
            }
            Some(old) => {
                adds.push(PlannedOp {
                    op: PlanOp::AddUser(new.clone()),
                    scope: new.scope,
                });
                user_removals.push(PlannedOp {
                    op: PlanOp::RemoveUser(*session),
                    scope: old.scope,
                });
            }
            None => adds.push(PlannedOp {
                op: PlanOp::AddUser(new.clone()),
                scope: new.scope,
            }),
        }
    }
    for (session, old) in &before.users {
        if !after.users.contains_key(session) {
            user_removals.push(PlannedOp {
                op: PlanOp::RemoveUser(*session),
                scope: old.scope,
            });
        }
    }

    let mut ops: Vec<PlannedOp> = Vec::new();
    for channel in order_creations(creates, before) {
        let scope = channel.scope;
        ops.push(PlannedOp {
            op: PlanOp::CreateChannel(channel),
            scope,
        });
    }
    ops.append(&mut updates);
    ops.append(&mut adds);
    ops.append(&mut moves);
    ops.append(&mut user_updates);
    ops.append(&mut user_removals);
    for (id, scope) in order_removals(channel_removals, before) {
        ops.push(PlannedOp {
            op: PlanOp::RemoveChannel(id),
            scope,
        });
    }
    ops
}

/// Overlay operations, split at the point [`crate::compose::splice`] needs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OverlayOps {
    /// Creations, moves and updates. Spliced **after** the shared additions, so
    /// they may target a channel that was created in the same turn.
    pub additions: Vec<PlanOp>,
    /// Withdrawals. Spliced **before** the shared removals, so they vacate a
    /// channel that is about to die.
    pub removals: Vec<PlanOp>,
}

impl OverlayOps {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.additions.is_empty() && self.removals.is_empty()
    }
}

/// Plan an overlay transition.
///
/// This is the planner without the whole-view checks: an overlay is not a
/// standalone view, so no root is required. The internal ordering rules are the
/// same - channels before users on the way in, users before channels on the way
/// out.
#[must_use]
pub fn plan_elements(before: &Overlay, after: &Overlay) -> OverlayOps {
    let mut creates: Vec<Channel> = Vec::new();
    let mut additions: Vec<PlanOp> = Vec::new();
    let mut trailing: Vec<PlanOp> = Vec::new();
    let mut removals: Vec<PlanOp> = Vec::new();
    let mut channel_removals: Vec<ChannelId> = Vec::new();

    for (id, new) in &after.channels {
        match before.channels.get(id) {
            Some(old) => {
                let patch = channel_patch(old, new);
                if !patch.is_noop() {
                    additions.push(PlanOp::UpdateChannel(patch));
                }
            }
            None => creates.push(new.clone()),
        }
    }
    for id in before.channels.keys() {
        if !after.channels.contains_key(id) {
            channel_removals.push(*id);
        }
    }

    for (session, new) in &after.users {
        match before.users.get(session) {
            Some(old) => {
                if old.channel != new.channel {
                    trailing.push(PlanOp::MoveUser {
                        session: *session,
                        channel: new.channel,
                    });
                }
                let patch = user_patch(old, new);
                if !patch.is_noop() {
                    trailing.push(PlanOp::UpdateUser(patch));
                }
            }
            None => trailing.push(PlanOp::AddUser(new.clone())),
        }
    }
    for session in before.users.keys() {
        if !after.users.contains_key(session) {
            removals.push(PlanOp::RemoveUser(*session));
        }
    }

    let mut ordered_additions: Vec<PlanOp> = Vec::new();
    for channel in order_creations(creates, &ShardView::empty()) {
        ordered_additions.push(PlanOp::CreateChannel(channel));
    }
    ordered_additions.append(&mut additions);
    ordered_additions.append(&mut trailing);

    // Users first so no occupied private channel is withdrawn, then channels
    // deepest first, using the overlay's own parent relation.
    let before_channels: BTreeMap<ChannelId, Channel> = before.channels.clone();
    let mut with_depth: Vec<(ChannelId, u32)> = channel_removals
        .into_iter()
        .map(|id| (id, overlay_depth(&before_channels, id)))
        .collect();
    with_depth.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
    removals.extend(
        with_depth
            .into_iter()
            .map(|(id, _)| PlanOp::RemoveChannel(id)),
    );

    OverlayOps {
        additions: ordered_additions,
        removals,
    }
}

fn channel_patch(old: &Channel, new: &Channel) -> ChannelPatch {
    let mut patch = ChannelPatch::empty(new.id);
    if old.parent != new.parent {
        patch.parent = Some(new.parent);
    }
    if old.name != new.name {
        patch.name = Some(new.name.clone());
    }
    if old.position != new.position {
        patch.position = Some(new.position);
    }
    if old.can_enter != new.can_enter {
        patch.can_enter = Some(new.can_enter);
    }
    patch.links_added = new.links.difference(&old.links).copied().collect();
    patch.links_removed = old.links.difference(&new.links).copied().collect();
    patch
}

fn user_patch(old: &User, new: &User) -> UserPatch {
    UserPatch {
        session: new.session,
        name: (old.name != new.name).then(|| new.name.clone()),
        flags: (old.flags != new.flags).then_some(new.flags),
    }
}

/// Order created channels so a parent always precedes its children.
///
/// A parent is either a channel already present in `before` or one created
/// earlier in this pass. The builder only produces trees, so this terminates;
/// the fallback exists so a malformed input cannot loop forever, and it only
/// affects ordering, which the caller has already validated.
fn order_creations(created: Vec<Channel>, before: &ShardView) -> Vec<Channel> {
    let mut present: HashSet<ChannelId> = before.channels.keys().copied().collect();
    let mut remaining: Vec<Channel> = created;
    let mut ordered: Vec<Channel> = Vec::with_capacity(remaining.len());

    while !remaining.is_empty() {
        let mut progressed = false;
        let mut waiting: Vec<Channel> = Vec::new();
        for channel in remaining {
            if channel.parent == channel.id || present.contains(&channel.parent) {
                present.insert(channel.id);
                ordered.push(channel);
                progressed = true;
            } else {
                waiting.push(channel);
            }
        }
        remaining = waiting;
        if !progressed {
            ordered.extend(remaining);
            break;
        }
    }
    ordered
}

/// Order removed channels so a child always precedes its parent, using the
/// previous view's parent relation. Ties break by id, for determinism.
fn order_removals(removed: Vec<(ChannelId, Scope)>, before: &ShardView) -> Vec<(ChannelId, Scope)> {
    let mut ordered = removed;
    ordered.sort_by(|left, right| {
        depth_in(before, right.0)
            .cmp(&depth_in(before, left.0))
            .then(left.0.cmp(&right.0))
    });
    ordered
}

fn depth_in(view: &ShardView, channel: ChannelId) -> u32 {
    let mut depth = 0u32;
    let mut current = channel;
    let mut guard: HashSet<ChannelId> = HashSet::new();
    while current != ChannelId::ROOT && guard.insert(current) {
        match view.channels.get(&current) {
            Some(entry) => {
                current = entry.parent;
                depth = depth.saturating_add(1);
            }
            None => break,
        }
    }
    depth
}

fn overlay_depth(channels: &BTreeMap<ChannelId, Channel>, channel: ChannelId) -> u32 {
    let mut depth = 0u32;
    let mut current = channel;
    let mut guard: HashSet<ChannelId> = HashSet::new();
    while guard.insert(current) {
        match channels.get(&current) {
            Some(entry) if entry.parent != current => {
                current = entry.parent;
                depth = depth.saturating_add(1);
            }
            // Either the parent is a shared channel, or the chain ends here.
            _ => break,
        }
    }
    depth
}
