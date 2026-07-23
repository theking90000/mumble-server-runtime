//! Logical diff between two views (spec 12.4).
//!
//! [`diff`] compares a committed view with a desired view and yields a
//! [`ViewDelta`]: the set of channel/user additions, field-level patches, and
//! removals, plus permission, context-action and listener changes. The delta is
//! purely structural; ordering it into a safe transition is [`crate::plan`]'s
//! job (spec 12.5).
//!
//! Patches carry only changed fields (`Some` means "changed to this value";
//! `Some(None)` clears a nullable field), so the planner can tell a user *move*
//! (a changed channel, spec 12.5 step 5) from other state changes (step 6), and
//! so applying a patch reproduces the desired element exactly.
//!
//! REF: docs/voxloom-specification-technique-v0.1.md 12.4 (`struct ViewDelta`).

use std::collections::BTreeSet;

use voxloom_render::{
    ActionKey, BlobRef, ChannelId, ClientView, ContextActionView, ListenerRelation, PermissionBits,
    SessionId, ViewChannel, ViewUser,
};

/// Field-level change to an existing channel. Every `Some` field replaces the
/// committed value; `None` means unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelPatch {
    pub id: ChannelId,
    pub parent: Option<ChannelId>,
    pub name: Option<String>,
    pub description: Option<Option<BlobRef>>,
    pub position: Option<i32>,
    pub temporary: Option<bool>,
    pub max_users: Option<Option<u32>>,
    pub enter_restricted: Option<bool>,
    pub can_enter: Option<bool>,
    pub links: Option<BTreeSet<ChannelId>>,
}

impl ChannelPatch {
    fn empty(id: ChannelId) -> ChannelPatch {
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

    /// True when no field changed (the two channels were equal).
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.parent.is_none()
            && self.name.is_none()
            && self.description.is_none()
            && self.position.is_none()
            && self.temporary.is_none()
            && self.max_users.is_none()
            && self.enter_restricted.is_none()
            && self.can_enter.is_none()
            && self.links.is_none()
    }
}

/// Field-level change to an existing user. A `Some` `channel` is a move (planned
/// separately from other state, spec 12.5 steps 5 and 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserPatch {
    pub session: SessionId,
    pub channel: Option<ChannelId>,
    pub name: Option<String>,
    pub user_id: Option<Option<u32>>,
    pub certificate_hash: Option<Option<String>>,
    pub mute: Option<bool>,
    pub deaf: Option<bool>,
    pub suppress: Option<bool>,
    pub self_mute: Option<bool>,
    pub self_deaf: Option<bool>,
    pub priority_speaker: Option<bool>,
    pub recording: Option<bool>,
    pub comment: Option<Option<BlobRef>>,
    pub texture: Option<Option<BlobRef>>,
}

impl UserPatch {
    fn empty(session: SessionId) -> UserPatch {
        UserPatch {
            session,
            channel: None,
            name: None,
            user_id: None,
            certificate_hash: None,
            mute: None,
            deaf: None,
            suppress: None,
            self_mute: None,
            self_deaf: None,
            priority_speaker: None,
            recording: None,
            comment: None,
            texture: None,
        }
    }

    /// True when no field changed.
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.channel.is_none()
            && self.name.is_none()
            && self.user_id.is_none()
            && self.certificate_hash.is_none()
            && self.mute.is_none()
            && self.deaf.is_none()
            && self.suppress.is_none()
            && self.self_mute.is_none()
            && self.self_deaf.is_none()
            && self.priority_speaker.is_none()
            && self.recording.is_none()
            && self.comment.is_none()
            && self.texture.is_none()
    }
}

/// New effective permission mask for a connection on a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionUpdate {
    pub channel: ChannelId,
    pub permissions: PermissionBits,
}

/// A listener relation turned on (`active`) or off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenerUpdate {
    pub relation: ListenerRelation,
    pub active: bool,
}

/// The logical difference between a committed and a desired view (spec 12.4).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ViewDelta {
    pub channels_added: Vec<ViewChannel>,
    pub channels_updated: Vec<ChannelPatch>,
    pub channels_removed: Vec<ChannelId>,

    pub users_added: Vec<ViewUser>,
    pub users_updated: Vec<UserPatch>,
    pub users_removed: Vec<SessionId>,

    pub permissions_updated: Vec<PermissionUpdate>,
    pub actions_added: Vec<ContextActionView>,
    pub actions_removed: Vec<ActionKey>,
    pub listener_updates: Vec<ListenerUpdate>,
}

impl ViewDelta {
    /// True when the two views were identical (nothing to transmit).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.channels_added.is_empty()
            && self.channels_updated.is_empty()
            && self.channels_removed.is_empty()
            && self.users_added.is_empty()
            && self.users_updated.is_empty()
            && self.users_removed.is_empty()
            && self.permissions_updated.is_empty()
            && self.actions_added.is_empty()
            && self.actions_removed.is_empty()
            && self.listener_updates.is_empty()
    }
}

/// Compute the logical delta from `committed` to `desired` (spec 12.4).
///
/// Both views are assumed normalized (spec 12.2 pipeline: normalize precedes
/// diff). Output order is deterministic: additions and updates follow the
/// ordered-map iteration of the input views.
#[must_use]
pub fn diff(committed: &ClientView, desired: &ClientView) -> ViewDelta {
    let mut delta = ViewDelta::default();

    // Channels.
    for (id, channel) in &desired.channels {
        match committed.channels.get(id) {
            None => delta.channels_added.push(channel.clone()),
            Some(old) => {
                let patch = channel_patch(old, channel);
                if !patch.is_noop() {
                    delta.channels_updated.push(patch);
                }
            }
        }
    }
    for id in committed.channels.keys() {
        if !desired.channels.contains_key(id) {
            delta.channels_removed.push(*id);
        }
    }

    // Users.
    for (session, user) in &desired.users {
        match committed.users.get(session) {
            None => delta.users_added.push(user.clone()),
            Some(old) => {
                let patch = user_patch(old, user);
                if !patch.is_noop() {
                    delta.users_updated.push(patch);
                }
            }
        }
    }
    for session in committed.users.keys() {
        if !desired.users.contains_key(session) {
            delta.users_removed.push(*session);
        }
    }

    // Effective permissions, only for channels that remain visible.
    for (channel, bits) in &desired.permissions {
        if committed.permissions.get(channel) != Some(bits) {
            delta.permissions_updated.push(PermissionUpdate {
                channel: *channel,
                permissions: *bits,
            });
        }
    }
    for channel in committed.permissions.keys() {
        if desired.channels.contains_key(channel) && !desired.permissions.contains_key(channel) {
            // Present before, absent now: the mask dropped to NONE.
            delta.permissions_updated.push(PermissionUpdate {
                channel: *channel,
                permissions: PermissionBits::NONE,
            });
        }
    }

    // Context actions: no update variant in ViewDelta, so a changed action is a
    // republish (added), a vanished one a removal.
    for (key, action) in &desired.context_actions {
        if committed.context_actions.get(key) != Some(action) {
            delta.actions_added.push(action.clone());
        }
    }
    for key in committed.context_actions.keys() {
        if !desired.context_actions.contains_key(key) {
            delta.actions_removed.push(key.clone());
        }
    }

    // Listeners: symmetric difference of the two relation sets.
    for relation in desired.listeners.difference(&committed.listeners) {
        delta.listener_updates.push(ListenerUpdate {
            relation: *relation,
            active: true,
        });
    }
    for relation in committed.listeners.difference(&desired.listeners) {
        delta.listener_updates.push(ListenerUpdate {
            relation: *relation,
            active: false,
        });
    }

    delta
}

fn channel_patch(old: &ViewChannel, new: &ViewChannel) -> ChannelPatch {
    let mut patch = ChannelPatch::empty(new.id);
    if old.parent != new.parent {
        patch.parent = Some(new.parent);
    }
    if old.name != new.name {
        patch.name = Some(new.name.clone());
    }
    if old.description != new.description {
        patch.description = Some(new.description.clone());
    }
    if old.position != new.position {
        patch.position = Some(new.position);
    }
    if old.temporary != new.temporary {
        patch.temporary = Some(new.temporary);
    }
    if old.max_users != new.max_users {
        patch.max_users = Some(new.max_users);
    }
    if old.enter_restricted != new.enter_restricted {
        patch.enter_restricted = Some(new.enter_restricted);
    }
    if old.can_enter != new.can_enter {
        patch.can_enter = Some(new.can_enter);
    }
    if old.links != new.links {
        patch.links = Some(new.links.clone());
    }
    patch
}

fn user_patch(old: &ViewUser, new: &ViewUser) -> UserPatch {
    let mut patch = UserPatch::empty(new.session);
    if old.channel != new.channel {
        patch.channel = Some(new.channel);
    }
    if old.name != new.name {
        patch.name = Some(new.name.clone());
    }
    if old.user_id != new.user_id {
        patch.user_id = Some(new.user_id);
    }
    if old.certificate_hash != new.certificate_hash {
        patch.certificate_hash = Some(new.certificate_hash.clone());
    }
    if old.mute != new.mute {
        patch.mute = Some(new.mute);
    }
    if old.deaf != new.deaf {
        patch.deaf = Some(new.deaf);
    }
    if old.suppress != new.suppress {
        patch.suppress = Some(new.suppress);
    }
    if old.self_mute != new.self_mute {
        patch.self_mute = Some(new.self_mute);
    }
    if old.self_deaf != new.self_deaf {
        patch.self_deaf = Some(new.self_deaf);
    }
    if old.priority_speaker != new.priority_speaker {
        patch.priority_speaker = Some(new.priority_speaker);
    }
    if old.recording != new.recording {
        patch.recording = Some(new.recording);
    }
    if old.comment != new.comment {
        patch.comment = Some(new.comment.clone());
    }
    if old.texture != new.texture {
        patch.texture = Some(new.texture.clone());
    }
    patch
}
