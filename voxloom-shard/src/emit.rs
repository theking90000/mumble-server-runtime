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

/// Effective-permission bits, as the Mumble client understands them.
///
/// REF: references/mumble/src/ACL.h : `enum ChanACL::Perm`.
pub mod perm {
    pub const TRAVERSE: u32 = 0x2;
    pub const ENTER: u32 = 0x4;
    pub const SPEAK: u32 = 0x8;
    pub const WHISPER: u32 = 0x100;
    pub const TEXT_MESSAGE: u32 = 0x200;

    /// What a client is told it may do where nothing forbids it.
    ///
    /// Deliberately excludes channel and administration rights: the tree is
    /// rendered by a flavor and nothing a client sends can edit it, so
    /// advertising those bits would only put buttons in the UI that answer with
    /// `PermissionDenied`.
    pub const DEFAULT: u32 = TRAVERSE | ENTER | SPEAK | WHISPER | TEXT_MESSAGE;
}

/// What a connection may do in a channel it can see, derived from the render.
///
/// There is no permission model to consult: the flavor renders a tree, and the
/// only thing it says about a channel's accessibility is `can_enter`. Deriving
/// the answer from that is what keeps the reply honest for the generation it was
/// asked about, rather than replaying a cache nothing invalidates (spec 16.16).
///
/// `TRAVERSE` is unconditional here because the question is only ever asked
/// about a channel the connection already observes: it has traversed it by
/// definition.
#[must_use]
pub fn permissions_of(channel: &Channel) -> u32 {
    if channel.can_enter {
        perm::DEFAULT
    } else {
        perm::DEFAULT & !perm::ENTER
    }
}

/// The reply to a client's `PermissionQuery` about one visible channel.
#[must_use]
pub fn permission_query(channel: &Channel) -> ControlMessage {
    ControlMessage::PermissionQuery(tcp::PermissionQuery {
        channel_id: Some(channel.id.0),
        permissions: Some(permissions_of(channel)),
        // A flush would tell the client to drop what it knows about **every**
        // channel. This answers one question about one channel.
        //
        // REF: references/mumble/src/mumble/Messages.cpp : `msgPermissionQuery`
        //   zeroes every channel's permissions when `flush()` is set.
        flush: Some(false),
    })
}

/// The reply to a client's `UserStats` about **somebody else** it can see.
///
/// It names the session and stops there. Everything the reference server puts in
/// this message - certificate chain, client version, IP address, packet
/// counters, connected and idle times - is either something this runtime does
/// not know or something spec 16.17 forbids disclosing without an explicit
/// authorization no flavor can currently express. An empty information window is
/// the honest rendering of "the server publishes nothing about this user".
///
/// The requester's own statistics are a different question, answered where the
/// transport lives rather than here.
///
/// REF: references/mumble/src/murmur/Messages.cpp : `msgUserStats` gates the
///   certificates, version and address behind `extend` (self, or Ban at the
///   root) and the counters behind `local`, and always answers with the session.
#[must_use]
pub fn user_stats(session: SessionId) -> ControlMessage {
    ControlMessage::UserStats(tcp::UserStats {
        session: Some(session.0),
        ..Default::default()
    })
}

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
    fn a_channel_nobody_may_enter_is_advertised_without_the_enter_bit() {
        let mut open = channel(1, 0);
        assert_eq!(permissions_of(&open), perm::DEFAULT);

        open.can_enter = false;
        let closed = permissions_of(&open);
        assert_eq!(
            closed & perm::ENTER,
            0,
            "the client must not offer to enter"
        );
        assert_eq!(
            closed & perm::TRAVERSE,
            perm::TRAVERSE,
            "a channel it can see is a channel it has traversed"
        );
    }

    #[test]
    fn a_permission_answer_never_flushes_the_client_s_cache() {
        let ControlMessage::PermissionQuery(answer) = permission_query(&channel(4, 0)) else {
            panic!("expected a PermissionQuery");
        };
        assert_eq!(answer.channel_id, Some(4));
        assert_eq!(answer.permissions, Some(perm::DEFAULT));
        assert_eq!(
            answer.flush,
            Some(false),
            "answering one question must not invalidate every other channel"
        );
    }

    #[test]
    fn stats_about_somebody_else_carry_the_session_and_nothing_more() {
        let ControlMessage::UserStats(answer) = user_stats(SessionId(7)) else {
            panic!("expected a UserStats");
        };
        assert_eq!(answer.session, Some(7));
        assert!(answer.certificates.is_empty(), "no certificate ever leaves");
        assert_eq!(answer.address, None, "no address ever leaves");
        assert_eq!(answer.version, None);
        assert_eq!(answer.from_client, None, "no counters about a third party");
        assert_eq!(
            answer.onlinesecs, None,
            "no connection time about a third party"
        );
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
