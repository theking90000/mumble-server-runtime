//! Translating a composed transition into Mumble control messages.
//!
//! [`crate::plan`] deliberately stops at view mutations: a [`PlanOp`] says *what
//! changes*, never *which frame carries it*. This module is the only place in
//! the crate that knows both vocabularies, and keeping it pure - no socket, no
//! queue - is what lets the ordering rules be tested against the message stream
//! itself.
//!
//! # The one constraint the plan cannot express
//!
//! A connection must be introduced to itself before it is introduced to anyone
//! else, because `ServerSync` makes the client look its own session up. The plan
//! has no notion of "self" - that is a connection notion, not a view notion - so
//! the rule is applied here, by reordering **inside a run of consecutive
//! `AddUser` operations** and never across the whole sequence. Hoisting the self
//! user to the front would place it before the `CreateChannel` of its own
//! channel, breaking one rule to satisfy another. The planner groups user
//! additions together, so a run is exactly the set among which order is free.

use voxloom_protocol::ControlMessage;
use voxloom_protocol::messages::tcp;

use crate::ids::{ChannelId, SessionId};
use crate::plan::{ChannelPatch, PlanOp, UserPatch};
use crate::view::{Channel, User};

/// Translate a composed transition into ordered control messages.
///
/// `self_session` is the connection's own session, used only for the
/// introduce-yourself-first rule.
#[must_use]
pub fn emit(ops: &[PlanOp], self_session: SessionId) -> Vec<ControlMessage> {
    self_first_within_user_additions(ops, self_session)
        .into_iter()
        .map(emit_op)
        .collect()
}

/// Reorder so that, inside each run of consecutive `AddUser` operations, the
/// connection introduces itself first.
fn self_first_within_user_additions(ops: &[PlanOp], self_session: SessionId) -> Vec<&PlanOp> {
    let mut ordered: Vec<&PlanOp> = Vec::with_capacity(ops.len());
    let mut index = 0;

    while index < ops.len() {
        let Some(op) = ops.get(index) else { break };
        if !matches!(op, PlanOp::AddUser(_)) {
            ordered.push(op);
            index += 1;
            continue;
        }

        let start = index;
        while ops
            .get(index)
            .is_some_and(|op| matches!(op, PlanOp::AddUser(_)))
        {
            index += 1;
        }
        let Some(run) = ops.get(start..index) else {
            break;
        };

        // Stable within each group, so a plan that never adds the self user
        // passes through completely unchanged.
        ordered.extend(run.iter().filter(|op| is_self_addition(op, self_session)));
        ordered.extend(run.iter().filter(|op| !is_self_addition(op, self_session)));
    }

    ordered
}

fn is_self_addition(op: &PlanOp, self_session: SessionId) -> bool {
    matches!(op, PlanOp::AddUser(user) if user.session == self_session)
}

/// No wildcard arm: adding a [`PlanOp`] variant must break this build rather
/// than silently produce a transition that omits it.
fn emit_op(op: &PlanOp) -> ControlMessage {
    match op {
        PlanOp::CreateChannel(channel) => ControlMessage::ChannelState(created_channel(channel)),
        PlanOp::UpdateChannel(patch) => ControlMessage::ChannelState(patched_channel(patch)),
        PlanOp::RemoveChannel(channel) => ControlMessage::ChannelRemove(tcp::ChannelRemove {
            channel_id: channel.0,
        }),
        PlanOp::AddUser(user) => ControlMessage::UserState(added_user(user)),
        PlanOp::UpdateUser(patch) => ControlMessage::UserState(patched_user(patch)),
        PlanOp::MoveUser { session, channel } => ControlMessage::UserState(tcp::UserState {
            session: Some(session.0),
            channel_id: Some(channel.0),
            ..Default::default()
        }),
        PlanOp::RemoveUser(session) => ControlMessage::UserRemove(tcp::UserRemove {
            session: session.0,
            ..Default::default()
        }),
    }
}

/// A newly created channel, sent whole.
///
/// The root carries no `parent`: the client refuses to create a channel without
/// one, and the root already exists on its side, so the message is an update to
/// something it has. A channel parented to itself would be a cycle.
///
/// REF: references/mumble/src/mumble/Messages.cpp : `msgChannelState` creates a
///   channel only `if (p && msg.has_name())`, and rejects a move into itself.
fn created_channel(channel: &Channel) -> tcp::ChannelState {
    let parent = (channel.id != ChannelId::ROOT).then_some(channel.parent.0);

    tcp::ChannelState {
        channel_id: Some(channel.id.0),
        parent,
        name: Some(channel.name.clone()),
        position: Some(channel.position),
        temporary: Some(false),
        can_enter: Some(channel.can_enter),
        links: channel.links.iter().map(|id| id.0).collect(),
        ..Default::default()
    }
}

/// A channel change, sent as a sparse `ChannelState`.
///
/// Links ride as `links_add`/`links_remove` rather than as `links`. The client
/// treats a non-empty `links` list as a full replacement and ignores an empty
/// one entirely, so incremental sets are both cheaper and the only way to
/// express "unlink the last one".
///
/// REF: references/mumble/src/mumble/Messages.cpp : `msgChannelState` handles
///   `links` under `if (msg.links_size())`, then `links_remove` and `links_add`
///   in their own independent blocks.
fn patched_channel(patch: &ChannelPatch) -> tcp::ChannelState {
    tcp::ChannelState {
        channel_id: Some(patch.id.0),
        parent: patch.parent.map(|id| id.0),
        name: patch.name.clone(),
        position: patch.position,
        can_enter: patch.can_enter,
        links_add: patch.links_added.iter().map(|id| id.0).collect(),
        links_remove: patch.links_removed.iter().map(|id| id.0).collect(),
        ..Default::default()
    }
}

/// A newly visible user, sent whole.
///
/// `channel_id` is always set, including for the root: Murmur omits it there,
/// but being explicit removes a class of bug where a user silently lands in the
/// root because a field was left out.
fn added_user(user: &User) -> tcp::UserState {
    let flags = user.flags;
    tcp::UserState {
        session: Some(user.session.0),
        name: Some(user.name.clone()),
        channel_id: Some(user.channel.0),
        mute: Some(flags.mute),
        deaf: Some(flags.deaf),
        suppress: Some(flags.suppress),
        self_mute: Some(flags.self_mute),
        self_deaf: Some(flags.self_deaf),
        priority_speaker: Some(flags.priority_speaker),
        recording: Some(flags.recording),
        ..Default::default()
    }
}

/// A user change, sent as a sparse `UserState`.
fn patched_user(patch: &UserPatch) -> tcp::UserState {
    let flags = patch.flags.unwrap_or_default();
    let set = patch.flags.is_some();

    tcp::UserState {
        session: Some(patch.session.0),
        name: patch.name.clone(),
        mute: set.then_some(flags.mute),
        deaf: set.then_some(flags.deaf),
        suppress: set.then_some(flags.suppress),
        self_mute: set.then_some(flags.self_mute),
        self_deaf: set.then_some(flags.self_deaf),
        priority_speaker: set.then_some(flags.priority_speaker),
        recording: set.then_some(flags.recording),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::collections::BTreeSet;

    use super::*;
    use crate::ids::{ChannelKey, ConnectionId, Occupant};
    use crate::scope::Scope;
    use crate::view::UserFlags;

    fn channel(id: u32, parent: u32) -> Channel {
        Channel {
            key: ChannelKey(u64::from(id)),
            id: ChannelId(id),
            parent: ChannelId(parent),
            scope: Scope::ROOT,
            name: format!("channel-{id}"),
            position: 0,
            can_enter: true,
            links: BTreeSet::new(),
        }
    }

    fn user(session: u32, channel: u32) -> User {
        User {
            occupant: Occupant::Connection(ConnectionId(u64::from(session))),
            session: SessionId(session),
            channel: ChannelId(channel),
            scope: Scope::ROOT,
            name: format!("user-{session}"),
            flags: UserFlags::default(),
        }
    }

    fn user_sessions(messages: &[ControlMessage]) -> Vec<u32> {
        messages
            .iter()
            .filter_map(|message| match message {
                ControlMessage::UserState(state) => state.session,
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_self_user_is_introduced_before_the_others() {
        let messages = emit(
            &[
                PlanOp::AddUser(user(7, 0)),
                PlanOp::AddUser(user(9, 0)),
                PlanOp::AddUser(user(3, 0)),
            ],
            SessionId(3),
        );
        assert_eq!(user_sessions(&messages), vec![3, 7, 9]);
    }

    #[test]
    fn the_self_user_is_not_hoisted_past_the_channel_it_lives_in() {
        let messages = emit(
            &[
                PlanOp::CreateChannel(channel(0, 0)),
                PlanOp::CreateChannel(channel(1, 0)),
                PlanOp::AddUser(user(7, 1)),
                PlanOp::AddUser(user(3, 1)),
            ],
            SessionId(3),
        );

        let kinds: Vec<&str> = messages
            .iter()
            .map(|message| match message {
                ControlMessage::ChannelState(_) => "channel",
                ControlMessage::UserState(_) => "user",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["channel", "channel", "user", "user"]);
    }

    #[test]
    fn a_plan_without_the_self_user_passes_through_unchanged() {
        let ops = [PlanOp::AddUser(user(7, 0)), PlanOp::AddUser(user(9, 0))];
        let messages = emit(&ops, SessionId(42));
        assert_eq!(user_sessions(&messages), vec![7, 9]);
    }

    #[test]
    fn the_root_channel_carries_no_parent() {
        let messages = emit(&[PlanOp::CreateChannel(channel(0, 0))], SessionId(1));
        let ControlMessage::ChannelState(state) = &messages[0] else {
            panic!("expected a ChannelState");
        };
        assert_eq!(state.parent, None, "a self-parented root is a cycle");
        assert_eq!(state.channel_id, Some(0));
    }

    #[test]
    fn a_child_channel_carries_its_parent_and_name() {
        // The client refuses to create a channel that has no parent or no name,
        // so both fields are load-bearing rather than cosmetic.
        let messages = emit(&[PlanOp::CreateChannel(channel(4, 1))], SessionId(1));
        let ControlMessage::ChannelState(state) = &messages[0] else {
            panic!("expected a ChannelState");
        };
        assert_eq!(state.parent, Some(1));
        assert_eq!(state.name.as_deref(), Some("channel-4"));
    }

    #[test]
    fn link_changes_ride_as_incremental_lists() {
        let patch = ChannelPatch {
            id: ChannelId(1),
            parent: None,
            name: None,
            position: None,
            can_enter: None,
            links_added: BTreeSet::from([ChannelId(4)]),
            links_removed: BTreeSet::from([ChannelId(2), ChannelId(3)]),
        };
        let messages = emit(&[PlanOp::UpdateChannel(patch)], SessionId(1));
        let ControlMessage::ChannelState(state) = &messages[0] else {
            panic!("expected a ChannelState");
        };

        assert_eq!(state.links_add, vec![4]);
        assert_eq!(state.links_remove, vec![2, 3]);
        assert!(
            state.links.is_empty(),
            "an empty `links` is a no-op client-side, so clearing must use links_remove"
        );
    }

    #[test]
    fn a_move_carries_only_the_session_and_the_channel() {
        let messages = emit(
            &[PlanOp::MoveUser {
                session: SessionId(5),
                channel: ChannelId(9),
            }],
            SessionId(1),
        );
        let ControlMessage::UserState(state) = &messages[0] else {
            panic!("expected a UserState");
        };
        assert_eq!(state.session, Some(5));
        assert_eq!(state.channel_id, Some(9));
        assert_eq!(state.name, None, "a move must not restate the name");
    }

    #[test]
    fn a_patch_with_no_flag_change_leaves_the_flags_unset() {
        let patch = UserPatch {
            session: SessionId(5),
            name: Some("renamed".to_owned()),
            flags: None,
        };
        let messages = emit(&[PlanOp::UpdateUser(patch)], SessionId(1));
        let ControlMessage::UserState(state) = &messages[0] else {
            panic!("expected a UserState");
        };
        assert_eq!(state.name.as_deref(), Some("renamed"));
        assert_eq!(
            state.mute, None,
            "sending a default flag would clear a flag nobody touched"
        );
    }

    #[test]
    fn removals_use_the_dedicated_messages() {
        let messages = emit(
            &[
                PlanOp::RemoveUser(SessionId(4)),
                PlanOp::RemoveChannel(ChannelId(9)),
            ],
            SessionId(1),
        );
        match messages.as_slice() {
            [
                ControlMessage::UserRemove(removed),
                ControlMessage::ChannelRemove(gone),
            ] => {
                assert_eq!(removed.session, 4);
                assert_eq!(gone.channel_id, 9);
            }
            other => panic!("unexpected emission: {other:?}"),
        }
    }
}
