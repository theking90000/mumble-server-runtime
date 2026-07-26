//! Translating an abstract transition plan into Mumble control messages.
//!
//! [`voxloom_reconcile::plan`] deliberately stops at view mutations: a `PlanOp`
//! says *what changes*, never *which frame carries it*. Turning those ops into
//! `ChannelState`/`UserState`/`ChannelRemove`/... is this module's whole job, and
//! it is the only place in the tree that knows both vocabularies. Keeping it
//! pure — no sockets, no queue, no connection — is what lets the ordering rules
//! be unit tested against the message stream itself.
//!
//! # One translation path, not two
//!
//! The initial sync is *not* a second implementation. The handshake stays a
//! session-lifecycle sequence with a plan-shaped hole in it:
//!
//! ```text
//! Version, CryptSetup, CodecVersion      <- session lifecycle
//!     emit_transaction(plan(empty, initial_view))
//! ServerSync, ServerConfig               <- session lifecycle
//! ```
//!
//! Those five messages carry no view mutation and therefore have no `PlanOp`;
//! they can only come from the session layer. What the plan cannot express on
//! its own is the ordering constraint that `ServerSync` implies: **the self user
//! must be introduced before the others** (spec 20 invariant 6). That rule is
//! enforced here, in [`emit_transaction`], and deliberately not in
//! `voxloom-reconcile`: "self" is a connection notion, not a view notion, and a
//! pure view engine that learns about it stops being one.
//!
//! # Fail closed rather than invent
//!
//! Two categories are refused outright instead of being approximated, and the
//! refusal takes down the whole transaction rather than half of it — which is
//! exactly the atomicity the output transaction already requires (invariant 20):
//!
//! - **Blob-backed fields** (channel description, user comment, user texture).
//!   The view carries a [`BlobRef`], an opaque handle the transport is supposed
//!   to resolve; there is no blob store yet, and inventing an encoding for the
//!   `*_hash` wire fields would advertise content the server cannot serve.
//! - **Clearing every channel link.** See [`channel_links`] for why that one
//!   needs the committed view, and why an empty `links` list is not the way.

use thiserror::Error;
use voxloom_protocol::ControlMessage;
use voxloom_protocol::messages::tcp;
use voxloom_reconcile::{
    AudioRoute, ChannelPatch, OutputTransaction, PermissionUpdate, PlanOp, UserPatch,
};
use voxloom_render::{
    ActionKey, ActionTarget, ChannelId, ClientView, ContextActionView, ListenerRelation,
    PermissionBits, SemanticKey, SessionId, ViewChannel, ViewUser,
};

/// One emitted step of a transition, in plan order.
///
/// Audio route toggles are steps rather than messages because they act on the
/// routing snapshot, not on the wire. Keeping them in the same ordered sequence
/// is what preserves spec 12.6: a revoked route is disabled before the view
/// changes (invariant 18) and a granted one is enabled after (invariant 19).
/// Splitting them into a separate list would lose exactly that ordering.
// No `Eq`: the generated protobuf messages carry float fields, so `ControlMessage`
// is only `PartialEq`.
//
// The variants are lopsided (a control message is ~328 bytes, a route toggle is
// 9) and boxing the large one is refused on purpose: a transition is dominated
// by messages, which the output queue already moves around unboxed, so boxing
// would buy padding on the handful of route toggles at the price of one
// allocation per emitted frame. This is the cold path; the trade is not worth it.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum EmittedStep {
    /// A control frame for this connection's output queue.
    Message(ControlMessage),
    /// An audio-plane action carrying no control frame.
    RouteChange { route: AudioRoute, enabled: bool },
}

/// Why a transition could not be put on the wire.
///
/// Every variant names the operation and the field, because "cannot emit" in a
/// log tells nobody which view element to look at.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EmitError {
    #[error(
        "{op}: field `{field}` is backed by a blob reference, which this phase cannot serve; \
         refusing the whole transition rather than sending a hash with no content behind it"
    )]
    BlobUnsupported {
        op: &'static str,
        field: &'static str,
    },

    #[error(
        "UpdateChannel({channel}): clearing every link needs the committed view to know what to \
         remove, and that channel is not in it"
    )]
    UnknownChannel { channel: u32 },
}

/// Translate a whole transition into ordered steps.
///
/// `committed` is the view this connection currently holds — the one the plan
/// was diffed against. It is needed to express removals that the wire encodes
/// as an explicit list rather than as a new value.
///
/// `self_session` is the connection's own session, used only to satisfy
/// invariant 6 when a transition introduces it alongside other users.
pub fn emit_transaction(
    transaction: &OutputTransaction,
    committed: &ClientView,
    self_session: SessionId,
) -> Result<Vec<EmittedStep>, EmitError> {
    let ops = self_first_within_user_additions(&transaction.ops, self_session);

    let mut steps = Vec::with_capacity(ops.len());
    for op in ops {
        emit_op(op, committed, &mut steps)?;
    }
    Ok(steps)
}

/// Reorder so that, inside each run of consecutive `AddUser` ops, the connection
/// introduces itself first (invariant 6).
///
/// The reordering is confined to a *run* on purpose. Hoisting the self user to
/// the front of the whole plan would place it before the `CreateChannel` that
/// its own channel needs, breaking invariant 5 to satisfy invariant 6. The
/// planner already groups user additions together (spec 12.5 addition step 4),
/// so a run is exactly the set among which order is free.
fn self_first_within_user_additions(ops: &[PlanOp], self_session: SessionId) -> Vec<&PlanOp> {
    let mut ordered: Vec<&PlanOp> = Vec::with_capacity(ops.len());
    let mut index = 0;

    while index < ops.len() {
        if !matches!(ops[index], PlanOp::AddUser(_)) {
            ordered.push(&ops[index]);
            index += 1;
            continue;
        }

        let start = index;
        while index < ops.len() && matches!(ops[index], PlanOp::AddUser(_)) {
            index += 1;
        }
        let run = &ops[start..index];

        // Stable within each group, so a plan that never adds the self user is
        // passed through completely unchanged.
        ordered.extend(run.iter().filter(|op| is_self_addition(op, self_session)));
        ordered.extend(run.iter().filter(|op| !is_self_addition(op, self_session)));
    }

    ordered
}

fn is_self_addition(op: &PlanOp, self_session: SessionId) -> bool {
    matches!(op, PlanOp::AddUser(user) if user.session == self_session)
}

fn emit_op(
    op: &PlanOp,
    committed: &ClientView,
    steps: &mut Vec<EmittedStep>,
) -> Result<(), EmitError> {
    // No wildcard arm: adding a `PlanOp` variant must break this build rather
    // than silently produce a transition that omits it.
    match op {
        PlanOp::DisableAudioRoute(route) => steps.push(EmittedStep::RouteChange {
            route: *route,
            enabled: false,
        }),
        PlanOp::EnableAudioRoute(route) => steps.push(EmittedStep::RouteChange {
            route: *route,
            enabled: true,
        }),

        PlanOp::CreateChannel(channel) => {
            steps.push(message(ControlMessage::ChannelState(created_channel(
                channel,
            )?)));
        }
        PlanOp::UpdateChannel(patch) => {
            steps.push(message(ControlMessage::ChannelState(patched_channel(
                patch, committed,
            )?)));
        }
        PlanOp::RemoveChannel(channel) => {
            steps.push(message(ControlMessage::ChannelRemove(tcp::ChannelRemove {
                channel_id: channel.0,
            })));
        }

        PlanOp::AddUser(user) => {
            steps.push(message(ControlMessage::UserState(added_user(user)?)));
        }
        PlanOp::UpdateUser(patch) => {
            steps.push(message(ControlMessage::UserState(patched_user(patch)?)));
        }
        PlanOp::MoveUser { session, channel } => {
            steps.push(message(ControlMessage::UserState(tcp::UserState {
                session: Some(session.0),
                channel_id: Some(channel.0),
                ..Default::default()
            })));
        }
        PlanOp::RemoveUser(session) => {
            steps.push(message(ControlMessage::UserRemove(tcp::UserRemove {
                session: session.0,
                ..Default::default()
            })));
        }

        PlanOp::AddListener(relation) => {
            steps.push(message(ControlMessage::UserState(listener_state(
                relation, true,
            ))));
        }
        PlanOp::RemoveListener(relation) => {
            steps.push(message(ControlMessage::UserState(listener_state(
                relation, false,
            ))));
        }

        PlanOp::UpdatePermissions(update) => {
            steps.push(message(ControlMessage::PermissionQuery(permission_query(
                update,
            ))));
        }

        PlanOp::AddAction(action) => {
            steps.push(message(ControlMessage::ContextActionModify(
                context_action(action, tcp::context_action_modify::Operation::Add),
            )));
        }
        PlanOp::RemoveAction(key) => {
            // A removal only needs the identifier; the client drops the entry by
            // key. REF: references/mumble/src/mumble/Messages.cpp :
            //   `msgContextActionModify` removes by `msg.action()`.
            steps.push(message(ControlMessage::ContextActionModify(
                tcp::ContextActionModify {
                    action: wire_action_key(key),
                    operation: Some(i32::from(tcp::context_action_modify::Operation::Remove)),
                    ..Default::default()
                },
            )));
        }
    }
    Ok(())
}

fn message(message: ControlMessage) -> EmittedStep {
    EmittedStep::Message(message)
}

/// A newly created channel, sent as a full `ChannelState`.
///
/// The root is the one channel that carries no `parent`: Murmur emits the field
/// only for channels that have one, and a channel parented to itself would be a
/// cycle for the client.
///
/// REF: references/mumble/src/murmur/Messages.cpp : `ChannelState` is built with
///   `if (c->cParent) mpcs.set_parent(...)`.
fn created_channel(channel: &ViewChannel) -> Result<tcp::ChannelState, EmitError> {
    if channel.description.is_some() {
        return Err(EmitError::BlobUnsupported {
            op: "CreateChannel",
            field: "description",
        });
    }

    let parent = if channel.id == ChannelId::ROOT {
        None
    } else {
        Some(channel.parent.0)
    };

    Ok(tcp::ChannelState {
        channel_id: Some(channel.id.0),
        parent,
        name: Some(channel.name.clone()),
        position: Some(channel.position),
        temporary: Some(channel.temporary),
        max_users: channel.max_users,
        is_enter_restricted: Some(channel.enter_restricted),
        can_enter: Some(channel.can_enter),
        links: channel.links.iter().map(|id| id.0).collect(),
        ..Default::default()
    })
}

/// A channel change, sent as a sparse `ChannelState` carrying only what moved.
fn patched_channel(
    patch: &ChannelPatch,
    committed: &ClientView,
) -> Result<tcp::ChannelState, EmitError> {
    if patch.description.is_some() {
        return Err(EmitError::BlobUnsupported {
            op: "UpdateChannel",
            field: "description",
        });
    }

    let (links, links_remove) = channel_links(patch, committed)?;

    Ok(tcp::ChannelState {
        channel_id: Some(patch.id.0),
        parent: patch.parent.map(|id| id.0),
        name: patch.name.clone(),
        position: patch.position,
        temporary: patch.temporary,
        max_users: patch.max_users.flatten(),
        is_enter_restricted: patch.enter_restricted,
        can_enter: patch.can_enter,
        links,
        links_remove,
        ..Default::default()
    })
}

/// Encode a new link set as the wire expects, returning `(links, links_remove)`.
///
/// The trap this exists for: the client treats `links` as a full replacement,
/// but only when the list is **non-empty** — it guards the whole branch on
/// `msg.links_size()`. An empty `links` is therefore a no-op, not "unlink
/// everything", so clearing the last link has to be expressed as an explicit
/// `links_remove` of what the connection currently holds. That is the only
/// reason this function needs the committed view.
///
/// REF: references/mumble/src/mumble/Messages.cpp : `msgChannelState` calls
///   `unlinkAll(c)` then relinks, inside `if (msg.links_size())`.
fn channel_links(
    patch: &ChannelPatch,
    committed: &ClientView,
) -> Result<(Vec<u32>, Vec<u32>), EmitError> {
    let Some(desired) = patch.links.as_ref() else {
        return Ok((Vec::new(), Vec::new()));
    };

    if !desired.is_empty() {
        return Ok((desired.iter().map(|id| id.0).collect(), Vec::new()));
    }

    let current = committed
        .channels
        .get(&patch.id)
        .ok_or(EmitError::UnknownChannel {
            channel: patch.id.0,
        })?;
    Ok((Vec::new(), current.links.iter().map(|id| id.0).collect()))
}

/// A newly visible user, sent as a full `UserState`.
///
/// `channel_id` is always set, including for the root. Murmur omits it there,
/// but setting it is equivalent for the client — it looks the channel up and
/// moves the user — and being explicit removes a class of bug where a user
/// silently lands in the root because a field was left out.
///
/// REF: references/mumble/src/mumble/Messages.cpp : `msgUserState` applies the
///   channel with `if (msg.has_channel_id())`, looking the channel up by id.
fn added_user(user: &ViewUser) -> Result<tcp::UserState, EmitError> {
    if user.comment.is_some() {
        return Err(EmitError::BlobUnsupported {
            op: "AddUser",
            field: "comment",
        });
    }
    if user.texture.is_some() {
        return Err(EmitError::BlobUnsupported {
            op: "AddUser",
            field: "texture",
        });
    }

    Ok(tcp::UserState {
        session: Some(user.session.0),
        name: Some(user.name.clone()),
        channel_id: Some(user.channel.0),
        user_id: user.user_id,
        hash: user.certificate_hash.clone(),
        mute: Some(user.mute),
        deaf: Some(user.deaf),
        suppress: Some(user.suppress),
        self_mute: Some(user.self_mute),
        self_deaf: Some(user.self_deaf),
        priority_speaker: Some(user.priority_speaker),
        recording: Some(user.recording),
        ..Default::default()
    })
}

/// A user change, sent as a sparse `UserState`.
fn patched_user(patch: &UserPatch) -> Result<tcp::UserState, EmitError> {
    if patch.comment.is_some() {
        return Err(EmitError::BlobUnsupported {
            op: "UpdateUser",
            field: "comment",
        });
    }
    if patch.texture.is_some() {
        return Err(EmitError::BlobUnsupported {
            op: "UpdateUser",
            field: "texture",
        });
    }

    Ok(tcp::UserState {
        session: Some(patch.session.0),
        name: patch.name.clone(),
        channel_id: patch.channel.map(|id| id.0),
        user_id: patch.user_id.flatten(),
        hash: patch.certificate_hash.clone().flatten(),
        mute: patch.mute,
        deaf: patch.deaf,
        suppress: patch.suppress,
        self_mute: patch.self_mute,
        self_deaf: patch.self_deaf,
        priority_speaker: patch.priority_speaker,
        recording: patch.recording,
        ..Default::default()
    })
}

/// A listener relation toggled on or off, carried on the listening user's own
/// `UserState` as an incremental add/remove list.
///
/// REF: references/vendored/Mumble.proto : `UserState.listening_channel_add` and
///   `listening_channel_remove`.
fn listener_state(relation: &ListenerRelation, listening: bool) -> tcp::UserState {
    let channel = relation.channel.0;
    let (add, remove) = if listening {
        (vec![channel], Vec::new())
    } else {
        (Vec::new(), vec![channel])
    };

    tcp::UserState {
        session: Some(relation.user.0),
        listening_channel_add: add,
        listening_channel_remove: remove,
        ..Default::default()
    }
}

/// The effective permission mask for one channel.
///
/// REF: references/mumble/src/murmur/Server.cpp : `Server::sendClientPermission`
///   sends a `PermissionQuery` carrying `channel_id` and `permissions`.
fn permission_query(update: &PermissionUpdate) -> tcp::PermissionQuery {
    tcp::PermissionQuery {
        channel_id: Some(update.channel.0),
        permissions: Some(wire_permissions(update.permissions)),
        ..Default::default()
    }
}

/// Map the canonical permission layout onto Mumble's ACL bits.
///
/// The two encodings agree for the first twelve flags and then diverge: the
/// reference reserves `0x1000`..`0x8000` and restarts the root-channel-only
/// permissions at `0x10000`, while the canonical layout keeps counting from bit
/// 12. Shifting the whole mask would therefore silently grant `Kick` to someone
/// who was given `Listen`.
///
/// REF: references/mumble/src/ACL.h : `enum Perm` (Write 0x1 .. Listen 0x800,
///   then Kick 0x10000 .. ResetUserContent 0x100000).
pub fn wire_permissions(permissions: PermissionBits) -> u32 {
    const MAPPING: [(u32, u32); 17] = [
        (PermissionBits::WRITE, 0x1),
        (PermissionBits::TRAVERSE, 0x2),
        (PermissionBits::ENTER, 0x4),
        (PermissionBits::SPEAK, 0x8),
        (PermissionBits::MUTE_DEAFEN, 0x10),
        (PermissionBits::MOVE, 0x20),
        (PermissionBits::MAKE_CHANNEL, 0x40),
        (PermissionBits::LINK_CHANNEL, 0x80),
        (PermissionBits::WHISPER, 0x100),
        (PermissionBits::TEXT_MESSAGE, 0x200),
        (PermissionBits::MAKE_TEMP_CHANNEL, 0x400),
        (PermissionBits::LISTEN, 0x800),
        (PermissionBits::KICK, 0x10000),
        (PermissionBits::BAN, 0x20000),
        (PermissionBits::REGISTER, 0x40000),
        (PermissionBits::SELF_REGISTER, 0x80000),
        (PermissionBits::RESET_USER_CONTENT, 0x100000),
    ];

    let mut wire = 0;
    for (canonical, reference) in MAPPING {
        if permissions.contains(canonical) {
            wire |= reference;
        }
    }
    wire
}

/// A context action offered or withdrawn.
fn context_action(
    action: &ContextActionView,
    operation: tcp::context_action_modify::Operation,
) -> tcp::ContextActionModify {
    // REF: references/vendored/Mumble.proto : `ContextActionModify.context` is a
    //   bitmask of `Context` (Server=1, Channel=2, User=4).
    let context = match action.target {
        ActionTarget::Server => tcp::context_action_modify::Context::Server,
        ActionTarget::Channel => tcp::context_action_modify::Context::Channel,
        ActionTarget::User => tcp::context_action_modify::Context::User,
    };

    tcp::ContextActionModify {
        action: wire_action_key(&action.key),
        text: Some(action.label.clone()),
        context: Some(u32::try_from(i32::from(context)).unwrap_or(0)),
        operation: Some(i32::from(operation)),
    }
}

/// Encode an action key as the opaque identifier the client echoes back.
///
/// The wire only requires an opaque string, so the encoding is ours to choose —
/// but it must be **injective**, because the inbound direction resolves a
/// client-supplied string back to a key before it may touch anything. The
/// one-character tag makes the two variants disjoint whatever their payload.
fn wire_action_key(key: &ActionKey) -> String {
    match &key.0 {
        SemanticKey::Static(name) => format!("s:{name}"),
        SemanticKey::Dynamic(id) => format!("d:{id}"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use voxloom_render::{ChannelKey, UserKey};

    fn channel(id: u32, parent: u32, name: &str) -> ViewChannel {
        ViewChannel {
            key: ChannelKey(SemanticKey::Static(name.to_string())),
            id: ChannelId(id),
            parent: ChannelId(parent),
            name: name.to_string(),
            description: None,
            position: 0,
            temporary: false,
            max_users: None,
            enter_restricted: false,
            can_enter: true,
            links: BTreeSet::new(),
        }
    }

    fn user(session: u32, name: &str, channel: u32) -> ViewUser {
        ViewUser {
            key: UserKey(SemanticKey::Static(name.to_string())),
            session: SessionId(session),
            name: name.to_string(),
            channel: ChannelId(channel),
            user_id: None,
            certificate_hash: None,
            mute: false,
            deaf: false,
            suppress: false,
            self_mute: false,
            self_deaf: false,
            priority_speaker: false,
            recording: false,
            comment: None,
            texture: None,
        }
    }

    fn transaction(ops: Vec<PlanOp>) -> OutputTransaction {
        OutputTransaction {
            from_revision: 0,
            to_revision: 1,
            ops,
            next_view: ClientView::empty(),
        }
    }

    fn emit(ops: Vec<PlanOp>, self_session: u32) -> Vec<EmittedStep> {
        emit_transaction(
            &transaction(ops),
            &ClientView::empty(),
            SessionId(self_session),
        )
        .expect("transition is emittable")
    }

    fn messages(steps: &[EmittedStep]) -> Vec<&ControlMessage> {
        steps
            .iter()
            .filter_map(|step| match step {
                EmittedStep::Message(message) => Some(message),
                EmittedStep::RouteChange { .. } => None,
            })
            .collect()
    }

    #[test]
    fn the_self_user_is_introduced_before_the_others() {
        let steps = emit(
            vec![
                PlanOp::AddUser(user(7, "bob", 0)),
                PlanOp::AddUser(user(9, "carol", 0)),
                PlanOp::AddUser(user(3, "alice", 0)),
            ],
            3,
        );

        let sessions: Vec<u32> = messages(&steps)
            .iter()
            .filter_map(|message| match message {
                ControlMessage::UserState(state) => state.session,
                _ => None,
            })
            .collect();

        // Invariant 6: self first. The others keep their planned order.
        assert_eq!(sessions, vec![3, 7, 9]);
    }

    #[test]
    fn the_self_user_is_not_hoisted_past_the_channel_it_lives_in() {
        let steps = emit(
            vec![
                PlanOp::CreateChannel(channel(0, 0, "root")),
                PlanOp::CreateChannel(channel(1, 0, "team")),
                PlanOp::AddUser(user(7, "bob", 1)),
                PlanOp::AddUser(user(3, "alice", 1)),
            ],
            3,
        );

        let kinds: Vec<&str> = messages(&steps)
            .iter()
            .map(|message| match message {
                ControlMessage::ChannelState(_) => "channel",
                ControlMessage::UserState(_) => "user",
                _ => "other",
            })
            .collect();

        // Reordering self must stay inside the run of user additions: a user
        // before its channel would break invariant 5 to satisfy invariant 6.
        assert_eq!(kinds, vec!["channel", "channel", "user", "user"]);
    }

    #[test]
    fn audio_route_toggles_keep_their_place_around_the_view_change() {
        let route = AudioRoute {
            sender: SessionId(1),
            receiver: SessionId(2),
        };
        let steps = emit(
            vec![
                PlanOp::DisableAudioRoute(route),
                PlanOp::CreateChannel(channel(1, 0, "team")),
                PlanOp::EnableAudioRoute(route),
            ],
            1,
        );

        assert_eq!(
            steps,
            vec![
                EmittedStep::RouteChange {
                    route,
                    enabled: false
                },
                steps[1].clone(),
                EmittedStep::RouteChange {
                    route,
                    enabled: true
                },
            ],
            "spec 12.6 ordering must survive translation"
        );
        assert!(matches!(steps[1], EmittedStep::Message(_)));
    }

    #[test]
    fn the_root_channel_carries_no_parent() {
        let steps = emit(vec![PlanOp::CreateChannel(channel(0, 0, "root"))], 1);
        let ControlMessage::ChannelState(state) = messages(&steps)[0] else {
            panic!("expected a ChannelState");
        };
        assert_eq!(state.parent, None, "a self-parented root is a cycle");
        assert_eq!(state.channel_id, Some(0));
    }

    #[test]
    fn permission_bits_map_onto_the_reference_acl_values() {
        // Each canonical flag alone, against the value read from ACL.h.
        let expected = [
            (PermissionBits::WRITE, 0x1),
            (PermissionBits::TRAVERSE, 0x2),
            (PermissionBits::ENTER, 0x4),
            (PermissionBits::SPEAK, 0x8),
            (PermissionBits::MUTE_DEAFEN, 0x10),
            (PermissionBits::MOVE, 0x20),
            (PermissionBits::MAKE_CHANNEL, 0x40),
            (PermissionBits::LINK_CHANNEL, 0x80),
            (PermissionBits::WHISPER, 0x100),
            (PermissionBits::TEXT_MESSAGE, 0x200),
            (PermissionBits::MAKE_TEMP_CHANNEL, 0x400),
            (PermissionBits::LISTEN, 0x800),
            (PermissionBits::KICK, 0x10000),
            (PermissionBits::BAN, 0x20000),
            (PermissionBits::REGISTER, 0x40000),
            (PermissionBits::SELF_REGISTER, 0x80000),
            (PermissionBits::RESET_USER_CONTENT, 0x100000),
        ];

        for (canonical, reference) in expected {
            assert_eq!(
                wire_permissions(PermissionBits(canonical)),
                reference,
                "canonical flag {canonical:#x} must map to ACL value {reference:#x}"
            );
        }

        // The gap is the point: Listen and Kick are adjacent canonically and
        // three bits apart on the wire.
        assert_eq!(
            wire_permissions(PermissionBits(
                PermissionBits::LISTEN | PermissionBits::KICK
            )),
            0x800 | 0x10000
        );
    }

    #[test]
    fn a_non_empty_link_set_replaces_and_an_empty_one_removes() {
        let mut with_links = channel(1, 0, "team");
        with_links.links = BTreeSet::from([ChannelId(2), ChannelId(3)]);
        let committed = ClientView {
            channels: BTreeMap::from([(ChannelId(1), with_links)]),
            ..ClientView::empty()
        };

        let replace = ChannelPatch {
            links: Some(BTreeSet::from([ChannelId(4)])),
            ..empty_patch(ChannelId(1))
        };
        let (links, links_remove) =
            channel_links(&replace, &committed).expect("replacement is emittable");
        assert_eq!(links, vec![4]);
        assert!(links_remove.is_empty());

        // An empty `links` list is a no-op for the client, so clearing has to be
        // spelled out as an explicit removal of what is currently held.
        let clear = ChannelPatch {
            links: Some(BTreeSet::new()),
            ..empty_patch(ChannelId(1))
        };
        let (links, links_remove) =
            channel_links(&clear, &committed).expect("clearing is emittable");
        assert!(links.is_empty());
        assert_eq!(links_remove, vec![2, 3]);
    }

    #[test]
    fn a_blob_backed_field_refuses_the_whole_transition() {
        let mut described = channel(1, 0, "team");
        described.description = Some(voxloom_render::BlobRef("deadbeef".to_string()));

        let error = emit_transaction(
            &transaction(vec![
                PlanOp::CreateChannel(channel(0, 0, "root")),
                PlanOp::CreateChannel(described),
            ]),
            &ClientView::empty(),
            SessionId(1),
        )
        .expect_err("a blob reference cannot be put on the wire yet");

        assert_eq!(
            error,
            EmitError::BlobUnsupported {
                op: "CreateChannel",
                field: "description",
            }
        );
    }

    #[test]
    fn action_keys_encode_injectively() {
        let static_key = ActionKey(SemanticKey::Static("d:5".to_string()));
        let dynamic_key = ActionKey(SemanticKey::Dynamic(5));

        // The colliding payload is the interesting case: the tag has to keep
        // them apart, because the inbound path resolves this string back.
        assert_ne!(wire_action_key(&static_key), wire_action_key(&dynamic_key));
        assert_eq!(wire_action_key(&dynamic_key), "d:5");
    }

    #[test]
    fn removing_a_user_and_a_channel_uses_the_dedicated_messages() {
        let steps = emit(
            vec![
                PlanOp::RemoveUser(SessionId(4)),
                PlanOp::RemoveChannel(ChannelId(9)),
            ],
            1,
        );

        match messages(&steps).as_slice() {
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

    #[test]
    fn a_listener_relation_rides_on_the_listening_user() {
        let relation = ListenerRelation {
            user: SessionId(6),
            channel: ChannelId(2),
        };
        let steps = emit(
            vec![
                PlanOp::AddListener(relation),
                PlanOp::RemoveListener(relation),
            ],
            1,
        );

        match messages(&steps).as_slice() {
            [
                ControlMessage::UserState(added),
                ControlMessage::UserState(removed),
            ] => {
                assert_eq!(added.session, Some(6));
                assert_eq!(added.listening_channel_add, vec![2]);
                assert!(added.listening_channel_remove.is_empty());
                assert_eq!(removed.listening_channel_remove, vec![2]);
                assert!(removed.listening_channel_add.is_empty());
            }
            other => panic!("unexpected emission: {other:?}"),
        }
    }

    fn empty_patch(id: ChannelId) -> ChannelPatch {
        ChannelPatch {
            id,
            parent: None,
            name: None,
            description: None,
            position: None,
            temporary: None,
            max_users: None,
            enter_restricted: None,
            can_enter: None,
            links: None,
        }
    }
}
