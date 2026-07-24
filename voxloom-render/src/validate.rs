//! View validation: the section-20 invariants checkable on a single view.
//!
//! REF: docs/voxloom-specification-technique-v0.1.md 20 ("Invariants
//!      protocolaires"). Each invariant is a named [`Invariant`] and its own
//!      check, so a failure names the broken rule (spec 26.6: named assertions),
//!      and so a mutation test can delete one check and observe exactly one
//!      invariant stop firing.
//!
//! Scope. This crate validates a *static* view, so it enforces the invariants
//! that are a property of one view: 1 (root exists), 3 (child has a visible
//! parent), 4 (no parent cycle), 5 (user in a visible channel), 6 (self-user
//! present before `ServerSync`), 11 (ids unique / consistent), 15 (no invisible
//! actor referenced). The transition invariants (2, 8, 9, 10, 12, 18, 19, 20)
//! are properties of a *plan* and are enforced by `voxloom-reconcile`. Invariant
//! 7 (an outgoing audio session is a known user) is validated where audio routes
//! live (the reconciler's plan). Invariants 13, 14, 17 govern the inbound
//! command path (Phase 3/9) and are out of scope for the pure view engine.

use std::collections::BTreeSet;

use crate::ids::{ChannelId, SessionId};
use crate::view::ClientView;

/// A named section-20 invariant enforced on a single view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invariant {
    /// 1: the root channel exists.
    RootChannelExists,
    /// 3: every child channel has a visible parent.
    ChildHasVisibleParent,
    /// 4: the parent tree contains no cycle.
    NoParentCycle,
    /// 5: every visible user is in a visible channel.
    UserInVisibleChannel,
    /// 6: the self-user exists (before `ServerSync`).
    SelfUserPresent,
    /// 11: ids are unique and consistent with the elements they key.
    UniqueIds,
    /// 15: no invisible actor (user, channel) is referenced.
    NoInvisibleActorReferenced,
}

/// A validation failure: which invariant broke, and enough context to act.
#[derive(Debug, Clone, thiserror::Error)]
#[error("invariant {invariant:?} violated: {detail}")]
pub struct ValidationError {
    pub invariant: Invariant,
    pub detail: String,
}

impl ValidationError {
    fn new(invariant: Invariant, detail: impl Into<String>) -> ValidationError {
        ValidationError {
            invariant,
            detail: detail.into(),
        }
    }
}

/// Validate a single view against the static section-20 invariants.
///
/// `self_session` is the connection's own session id when the view is expected
/// to be post-`ServerSync` (invariant 6); pass `None` for the pre-sync/empty
/// view, which legitimately has no users yet.
///
/// Returns the first invariant violated. Checks run in prerequisite order (root,
/// then structure, then references) so a returned error names the most
/// fundamental break.
pub fn validate(view: &ClientView, self_session: Option<SessionId>) -> Result<(), ValidationError> {
    check_ids_consistent(view)?;
    check_root_exists(view)?;
    check_children_have_parents(view)?;
    check_no_parent_cycle(view)?;
    check_users_in_visible_channels(view)?;
    check_no_invisible_actor(view)?;
    check_self_present(view, self_session)?;
    Ok(())
}

/// Invariant 11.
fn check_ids_consistent(view: &ClientView) -> Result<(), ValidationError> {
    for (id, channel) in &view.channels {
        if channel.id != *id {
            return Err(ValidationError::new(
                Invariant::UniqueIds,
                format!("channel keyed by {id:?} declares id {:?}", channel.id),
            ));
        }
    }
    for (session, user) in &view.users {
        if user.session != *session {
            return Err(ValidationError::new(
                Invariant::UniqueIds,
                format!(
                    "user keyed by {session:?} declares session {:?}",
                    user.session
                ),
            ));
        }
    }
    Ok(())
}

/// Invariant 1.
fn check_root_exists(view: &ClientView) -> Result<(), ValidationError> {
    if view.channels.contains_key(&view.root_channel) {
        Ok(())
    } else {
        Err(ValidationError::new(
            Invariant::RootChannelExists,
            format!("root channel {:?} is not in the view", view.root_channel),
        ))
    }
}

/// Invariant 3. The root is its own parent and is exempt.
fn check_children_have_parents(view: &ClientView) -> Result<(), ValidationError> {
    for channel in view.channels.values() {
        if channel.id == view.root_channel {
            continue;
        }
        if !view.channels.contains_key(&channel.parent) {
            return Err(ValidationError::new(
                Invariant::ChildHasVisibleParent,
                format!(
                    "channel {:?} has parent {:?} which is not visible",
                    channel.id, channel.parent
                ),
            ));
        }
    }
    Ok(())
}

/// Invariant 4. Walk each channel's ancestry to the root; a revisited node is a
/// cycle. Only reachable once parents are known (call after invariant 3).
fn check_no_parent_cycle(view: &ClientView) -> Result<(), ValidationError> {
    for start in view.channels.keys() {
        let mut seen: BTreeSet<ChannelId> = BTreeSet::new();
        let mut current = *start;
        loop {
            if !seen.insert(current) {
                return Err(ValidationError::new(
                    Invariant::NoParentCycle,
                    format!("parent chain from {start:?} cycles at {current:?}"),
                ));
            }
            if current == view.root_channel {
                break;
            }
            match view.channels.get(&current) {
                Some(channel) => current = channel.parent,
                // Missing parent is invariant 3's job; stop walking here.
                None => break,
            }
        }
    }
    Ok(())
}

/// Invariant 5.
fn check_users_in_visible_channels(view: &ClientView) -> Result<(), ValidationError> {
    for user in view.users.values() {
        if !view.channels.contains_key(&user.channel) {
            return Err(ValidationError::new(
                Invariant::UserInVisibleChannel,
                format!(
                    "user {:?} is in channel {:?} which is not visible",
                    user.session, user.channel
                ),
            ));
        }
    }
    Ok(())
}

/// Invariant 15: listeners, channel links and permission entries must reference
/// only visible entities.
fn check_no_invisible_actor(view: &ClientView) -> Result<(), ValidationError> {
    for listener in &view.listeners {
        if !view.users.contains_key(&listener.user) {
            return Err(ValidationError::new(
                Invariant::NoInvisibleActorReferenced,
                format!("listener references invisible user {:?}", listener.user),
            ));
        }
        if !view.channels.contains_key(&listener.channel) {
            return Err(ValidationError::new(
                Invariant::NoInvisibleActorReferenced,
                format!(
                    "listener references invisible channel {:?}",
                    listener.channel
                ),
            ));
        }
    }
    for channel in view.channels.values() {
        for link in &channel.links {
            if !view.channels.contains_key(link) {
                return Err(ValidationError::new(
                    Invariant::NoInvisibleActorReferenced,
                    format!(
                        "channel {:?} links to invisible channel {link:?}",
                        channel.id
                    ),
                ));
            }
        }
    }
    for channel_id in view.permissions.keys() {
        if !view.channels.contains_key(channel_id) {
            return Err(ValidationError::new(
                Invariant::NoInvisibleActorReferenced,
                format!("permissions reference invisible channel {channel_id:?}"),
            ));
        }
    }
    Ok(())
}

/// Invariant 6.
fn check_self_present(
    view: &ClientView,
    self_session: Option<SessionId>,
) -> Result<(), ValidationError> {
    match self_session {
        Some(session) if !view.users.contains_key(&session) => Err(ValidationError::new(
            Invariant::SelfUserPresent,
            format!("self-user {session:?} is not present in the view"),
        )),
        _ => Ok(()),
    }
}
