//! The strict client model.
//!
//! [`ClientModel`] applies server control messages exactly as a conforming
//! Mumble client would, and **panics** the instant a message would violate one
//! of the spec §20 protocol invariants — the same way the official client
//! rejects a malformed server. It is the machine judge of the handshake and of
//! every later phase (roadmap P3, R2): it is written against the spec and the
//! vendored reference, never against the Voxloom server.
//!
//! Each invariant is enforced by its own named `check_*` method, so a mutation
//! that deletes one check is visible and testable in isolation.

use std::collections::{BTreeMap, BTreeSet};

use voxloom_protocol::messages::{tcp, udp};
use voxloom_protocol::{ControlMessage, UdpMessage, decode_udp};

/// The root channel is always id 0 (spec §11.2, §20 invariant 1).
pub const ROOT_CHANNEL_ID: u32 = 0;

/// A channel as the client models it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChannel {
    pub id: u32,
    /// Parent channel id; `None` for the root.
    pub parent: Option<u32>,
    pub name: String,
}

/// A user as the client models it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelUser {
    pub session: u32,
    pub name: String,
    /// The channel the user is shown in (root if the server omitted channel_id).
    pub channel: u32,
}

/// The client's view of the server, built by applying control messages.
#[derive(Debug, Default)]
pub struct ClientModel {
    /// This connection's own session id, learned from `ServerSync`.
    pub self_session: Option<u32>,
    /// Whether `ServerSync` has been received (the view is then "live").
    pub synced: bool,
    pub channels: BTreeMap<u32, ModelChannel>,
    pub users: BTreeMap<u32, ModelUser>,
    /// Effective root permissions from `ServerSync`.
    pub root_permissions: Option<u64>,
    retired_channels: BTreeSet<u32>,
    /// Sessions removed from this view, with the name they carried. A session
    /// coming back for the *same* identity is a projection making a user
    /// visible again, not id reuse; coming back under another name is the
    /// recycling invariant 12 forbids.
    retired_sessions: BTreeMap<u32, String>,
}

impl ClientModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one server->client control message, enforcing every §20 invariant it
    /// touches. Panics on any violation (the model's contract, §26.6).
    ///
    // REF: references/mumble/src/Mumble.proto:ChannelState, UserState, TextMessage,
    // PermissionDenied, ACL, ContextAction, UserList, VoiceTarget, PermissionQuery,
    // UserStats, RequestBlob, and PluginDataTransmission entity reference fields.
    pub fn apply(&mut self, message: &ControlMessage) {
        match message {
            ControlMessage::UdpTunnel(bytes) => {
                let message = decode_udp(bytes)
                    .unwrap_or_else(|error| panic!("invalid tunneled UDP message: {error}"));
                if let UdpMessage::Audio(audio) = message {
                    self.apply_audio(&audio);
                }
            }
            ControlMessage::ChannelState(cs) => self.apply_channel_state(cs),
            ControlMessage::ChannelRemove(cr) => self.apply_channel_remove(cr),
            ControlMessage::UserState(us) => self.apply_user_state(us),
            ControlMessage::UserRemove(ur) => self.apply_user_remove(ur),
            ControlMessage::ServerSync(sync) => self.apply_server_sync(sync),
            ControlMessage::TextMessage(message) => self.check_text_references(message),
            ControlMessage::PermissionDenied(message) => {
                self.check_optional_channel(message.channel_id, "PermissionDenied");
                self.check_optional_session(message.session, "PermissionDenied");
            }
            ControlMessage::Acl(message) => {
                self.check_channel_reference(message.channel_id, "ACL");
            }
            ControlMessage::ContextAction(message) => {
                self.check_optional_session(message.session, "ContextAction");
                self.check_optional_channel(message.channel_id, "ContextAction");
            }
            ControlMessage::UserList(message) => {
                for user in &message.users {
                    self.check_optional_channel(user.last_channel, "UserList.last_channel");
                }
            }
            ControlMessage::VoiceTarget(message) => {
                for target in &message.targets {
                    self.check_sessions(&target.session, "VoiceTarget.session");
                    self.check_optional_channel(target.channel_id, "VoiceTarget.channel_id");
                }
            }
            ControlMessage::PermissionQuery(message) => {
                self.check_optional_channel(message.channel_id, "PermissionQuery");
            }
            ControlMessage::UserStats(message) => {
                self.check_optional_session(message.session, "UserStats");
            }
            ControlMessage::RequestBlob(message) => {
                self.check_sessions(&message.session_texture, "RequestBlob.session_texture");
                self.check_sessions(&message.session_comment, "RequestBlob.session_comment");
                self.check_channels(
                    &message.channel_description,
                    "RequestBlob.channel_description",
                );
            }
            ControlMessage::PluginDataTransmission(message) => {
                self.check_optional_session(
                    message.sender_session,
                    "PluginDataTransmission.sender_session",
                );
                self.check_sessions(
                    &message.receiver_sessions,
                    "PluginDataTransmission.receiver_sessions",
                );
            }
            // These messages carry no channel or session reference.
            _ => {}
        }
    }

    /// Apply one server-to-client audio message.
    ///
    // REF: references/mumble/src/MumbleUDP.proto:Audio.sender_session
    // REF: references/mumble/src/mumble/ServerHandler.cpp:handleVoicePacket
    pub fn apply_audio(&self, audio: &udp::Audio) {
        self.check_session_reference(audio.sender_session, "Audio.sender_session");
    }

    fn apply_channel_state(&mut self, cs: &tcp::ChannelState) {
        let id = match cs.channel_id {
            Some(id) => id,
            // A ChannelState with no channel_id is not a create/update the client
            // can place; a conforming server never sends one during sync.
            None => panic!("§20: ChannelState without channel_id"),
        };

        let is_new = !self.channels.contains_key(&id);
        if is_new {
            if self.retired_channels.contains(&id) {
                panic!("§20 invariant 12: retired channel id {id} was reused");
            }
            // Invariant 3: a new child channel must reference an already-visible
            // parent (the root, id 0, is the only channel allowed to have none).
            self.check_parent_visible(id, cs.parent);
            if id != ROOT_CHANNEL_ID && cs.name.is_none() {
                panic!("§20: new channel {id} has no name");
            }
        } else {
            self.check_optional_channel(cs.parent, "ChannelState.parent");
        }
        self.check_channels(&cs.links, "ChannelState.links");
        self.check_channels(&cs.links_add, "ChannelState.links_add");
        self.check_channels(&cs.links_remove, "ChannelState.links_remove");

        let entry = self.channels.entry(id).or_insert_with(|| ModelChannel {
            id,
            parent: if id == ROOT_CHANNEL_ID {
                None
            } else {
                cs.parent
            },
            name: String::new(),
        });
        if let Some(parent) = cs.parent
            && id != ROOT_CHANNEL_ID
        {
            entry.parent = Some(parent);
        }
        if let Some(name) = &cs.name {
            entry.name = name.clone();
        }

        // Invariant 4: the parent chain must remain acyclic after the update.
        self.check_no_cycles();
    }

    fn apply_channel_remove(&mut self, cr: &tcp::ChannelRemove) {
        let id = cr.channel_id;
        // Invariant 2: the root channel is never removed.
        if id == ROOT_CHANNEL_ID {
            panic!("§20 invariant 2: server tried to remove the root channel");
        }
        self.check_channel_reference(id, "ChannelRemove.channel_id");
        // Invariant 8: an occupied channel is never removed.
        self.check_channel_empty(id);
        if let Some(child) = self
            .channels
            .values()
            .find(|channel| channel.parent == Some(id))
        {
            panic!(
                "§20 invariant 10: channel {id} removed before child {}",
                child.id
            );
        }
        self.channels.remove(&id);
        self.retired_channels.insert(id);
    }

    fn apply_user_state(&mut self, us: &tcp::UserState) {
        let session = match us.session {
            Some(session) => session,
            None => panic!("§20: UserState without a session id"),
        };
        let is_new = !self.users.contains_key(&session);
        if is_new && us.name.is_none() {
            panic!("§20: new session {session} has no name");
        }
        if is_new && let Some(retired) = self.retired_sessions.get(&session) {
            // A per-connection projection may hide a user and show it again
            // later; its session is stable for the whole connection (spec 9.1),
            // so the same identity returning is correct. Another identity on
            // that id is the reuse invariant 12 forbids, and would hand the
            // client's per-user local state to a stranger (spec 9.3).
            match &us.name {
                Some(name) if name == retired => {}
                Some(name) => panic!(
                    "§20 invariant 12: retired session id {session} was reused, `{retired}` \
                     became `{name}`"
                ),
                None => panic!("§20: new session {session} has no name"),
            }
        }

        // Invariant 15: an actor, if named, must be a visible session.
        self.check_optional_session(us.actor, "UserState.actor");
        self.check_channels(&us.listening_channel_add, "UserState.listening_channel_add");
        self.check_channels(
            &us.listening_channel_remove,
            "UserState.listening_channel_remove",
        );
        for adjustment in &us.listening_volume_adjustment {
            self.check_optional_channel(
                adjustment.listening_channel,
                "UserState.VolumeAdjustment.listening_channel",
            );
        }

        // The channel defaults to root when the server omits channel_id.
        let existing_channel = self.users.get(&session).map(|u| u.channel);
        let channel = us
            .channel_id
            .or(existing_channel)
            .unwrap_or(ROOT_CHANNEL_ID);

        // Invariant 5: a visible user must be in a visible channel.
        self.check_channel_visible(channel, session);

        let entry = self.users.entry(session).or_insert_with(|| ModelUser {
            session,
            name: String::new(),
            channel,
        });
        entry.channel = channel;
        if let Some(name) = &us.name {
            entry.name = name.clone();
        }
    }

    fn apply_user_remove(&mut self, ur: &tcp::UserRemove) {
        self.check_session_reference(ur.session, "UserRemove.session");
        self.check_optional_session(ur.actor, "UserRemove.actor");
        // Removing our own session is a disconnect; removing another is a normal
        // presence update. Either way, drop it from the view.
        let removed = self.users.remove(&ur.session);
        self.retired_sessions.insert(
            ur.session,
            removed.map(|user| user.name).unwrap_or_default(),
        );
    }

    fn apply_server_sync(&mut self, sync: &tcp::ServerSync) {
        let session = match sync.session {
            Some(session) => session,
            None => panic!("§20: ServerSync without a session id"),
        };
        // Invariant 1: the root channel must exist by sync time.
        if !self.channels.contains_key(&ROOT_CHANNEL_ID) {
            panic!("§20 invariant 1: ServerSync before the root channel (0) was created");
        }
        // Invariant 6: the self-user must be known before ServerSync.
        if !self.users.contains_key(&session) {
            panic!(
                "§20 invariant 6: ServerSync for session {session} but no self-user was sent first"
            );
        }
        self.self_session = Some(session);
        self.root_permissions = sync.permissions;
        self.synced = true;
    }

    // --- Named invariant checks (one per §20 rule the client can see) ---------

    /// Invariant 3: a newly created channel other than the root must reference a
    /// parent that is already visible.
    fn check_parent_visible(&self, id: u32, parent: Option<u32>) {
        if id == ROOT_CHANNEL_ID {
            return;
        }
        match parent {
            Some(parent) if self.channels.contains_key(&parent) => {}
            Some(parent) => panic!(
                "§20 invariant 3: channel {id} references parent {parent} which is not visible"
            ),
            None => panic!("§20 invariant 3: non-root channel {id} has no parent"),
        }
    }

    /// Invariant 5: a visible user must sit in a visible channel.
    fn check_channel_visible(&self, channel: u32, session: u32) {
        if !self.channels.contains_key(&channel) {
            panic!(
                "§20 invariant 5: user {session} placed in channel {channel} which is not visible"
            );
        }
    }

    /// Invariant 8: a channel with users in it is never removed.
    fn check_channel_empty(&self, channel: u32) {
        if let Some(user) = self.users.values().find(|u| u.channel == channel) {
            panic!(
                "§20 invariant 8: channel {channel} removed while occupied by session {}",
                user.session
            );
        }
    }

    /// Invariants 14 and 15: every referenced session is visible to this client.
    fn check_session_reference(&self, session: u32, context: &str) {
        if !self.users.contains_key(&session) {
            panic!("§20 invariants 14/15: {context} references invisible session {session}");
        }
    }

    fn check_optional_session(&self, session: Option<u32>, context: &str) {
        if let Some(session) = session {
            self.check_session_reference(session, context);
        }
    }

    fn check_sessions(&self, sessions: &[u32], context: &str) {
        for session in sessions {
            self.check_session_reference(*session, context);
        }
    }

    /// Invariant 14: every referenced channel is visible to this client.
    fn check_channel_reference(&self, channel: u32, context: &str) {
        if !self.channels.contains_key(&channel) {
            panic!("§20 invariant 14: {context} references invisible channel {channel}");
        }
    }

    fn check_optional_channel(&self, channel: Option<u32>, context: &str) {
        if let Some(channel) = channel {
            self.check_channel_reference(channel, context);
        }
    }

    fn check_channels(&self, channels: &[u32], context: &str) {
        for channel in channels {
            self.check_channel_reference(*channel, context);
        }
    }

    fn check_text_references(&self, message: &tcp::TextMessage) {
        self.check_optional_session(message.actor, "TextMessage.actor");
        self.check_sessions(&message.session, "TextMessage.session");
        self.check_channels(&message.channel_id, "TextMessage.channel_id");
        self.check_channels(&message.tree_id, "TextMessage.tree_id");
    }

    /// Invariant 4: the channel parent chain contains no cycle. Walks from every
    /// channel to the root, bounding the walk by the channel count.
    fn check_no_cycles(&self) {
        let limit = self.channels.len() + 1;
        for start in self.channels.keys() {
            let mut current = *start;
            let mut steps = 0;
            loop {
                if current == ROOT_CHANNEL_ID {
                    break;
                }
                let parent = match self.channels.get(&current).and_then(|c| c.parent) {
                    Some(parent) => parent,
                    // Parent not yet visible: not a cycle, just an incomplete view.
                    None => break,
                };
                steps += 1;
                if steps > limit {
                    panic!("§20 invariant 4: cycle detected in the channel parent chain");
                }
                current = parent;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(id: u32, parent: Option<u32>) -> ControlMessage {
        ControlMessage::ChannelState(tcp::ChannelState {
            channel_id: Some(id),
            parent,
            name: Some(format!("chan{id}")),
            ..Default::default()
        })
    }

    fn user(session: u32, channel: Option<u32>) -> ControlMessage {
        ControlMessage::UserState(tcp::UserState {
            session: Some(session),
            name: Some(format!("user{session}")),
            channel_id: channel,
            ..Default::default()
        })
    }

    fn sync(session: u32) -> ControlMessage {
        ControlMessage::ServerSync(tcp::ServerSync {
            session: Some(session),
            ..Default::default()
        })
    }

    fn synced_model() -> ClientModel {
        let mut model = ClientModel::new();
        model.apply(&channel(ROOT_CHANNEL_ID, None));
        model.apply(&user(1, Some(ROOT_CHANNEL_ID)));
        model.apply(&sync(1));
        model
    }

    fn rejects_invisible_reference(label: &str, message: ControlMessage) {
        let mut model = synced_model();
        let result = std::panic::catch_unwind(move || model.apply(&message));
        assert!(result.is_err(), "{label} accepted an invisible reference");
    }

    #[test]
    fn accepts_a_well_formed_minimal_handshake() {
        let mut model = ClientModel::new();
        model.apply(&channel(0, None));
        model.apply(&user(1, Some(0)));
        model.apply(&sync(1));
        assert!(model.synced);
        assert_eq!(model.self_session, Some(1));
        assert!(model.channels.contains_key(&0));
        assert_eq!(model.users.get(&1).map(|u| u.channel), Some(0));
    }

    #[test]
    fn user_channel_defaults_to_root_when_omitted() {
        let mut model = ClientModel::new();
        model.apply(&channel(0, None));
        model.apply(&user(2, None)); // no channel_id => root
        assert_eq!(
            model.users.get(&2).map(|u| u.channel),
            Some(ROOT_CHANNEL_ID)
        );
    }

    #[test]
    #[should_panic(expected = "invariant 1")]
    fn server_sync_before_root_panics() {
        let mut model = ClientModel::new();
        model.apply(&sync(1));
    }

    #[test]
    #[should_panic(expected = "invariant 6")]
    fn server_sync_without_self_user_panics() {
        let mut model = ClientModel::new();
        model.apply(&channel(0, None));
        model.apply(&sync(1)); // no UserState for session 1 was sent
    }

    #[test]
    #[should_panic(expected = "invariant 2")]
    fn removing_root_panics() {
        let mut model = ClientModel::new();
        model.apply(&channel(0, None));
        model.apply(&ControlMessage::ChannelRemove(tcp::ChannelRemove {
            channel_id: 0,
        }));
    }

    #[test]
    #[should_panic(expected = "invariant 3")]
    fn child_with_invisible_parent_panics() {
        let mut model = ClientModel::new();
        model.apply(&channel(0, None));
        model.apply(&channel(5, Some(99))); // parent 99 not visible
    }

    #[test]
    #[should_panic(expected = "invariant 5")]
    fn user_in_invisible_channel_panics() {
        let mut model = ClientModel::new();
        model.apply(&channel(0, None));
        model.apply(&user(1, Some(7))); // channel 7 not visible
    }

    #[test]
    #[should_panic(expected = "invariant 8")]
    fn removing_occupied_channel_panics() {
        let mut model = ClientModel::new();
        model.apply(&channel(0, None));
        model.apply(&channel(1, Some(0)));
        model.apply(&user(1, Some(1)));
        model.apply(&ControlMessage::ChannelRemove(tcp::ChannelRemove {
            channel_id: 1,
        }));
    }

    #[test]
    #[should_panic(expected = "invariants 14/15")]
    fn invisible_actor_panics() {
        let mut model = ClientModel::new();
        model.apply(&channel(0, None));
        model.apply(&ControlMessage::UserState(tcp::UserState {
            session: Some(1),
            actor: Some(42), // session 42 is not visible
            name: Some("user1".to_string()),
            channel_id: Some(0),
            ..Default::default()
        }));
    }

    #[test]
    #[should_panic(expected = "invariant 4")]
    fn cyclic_channel_move_panics() {
        let mut model = ClientModel::new();
        model.apply(&channel(ROOT_CHANNEL_ID, None));
        model.apply(&channel(1, Some(ROOT_CHANNEL_ID)));
        model.apply(&channel(2, Some(1)));
        model.apply(&ControlMessage::ChannelState(tcp::ChannelState {
            channel_id: Some(1),
            parent: Some(2),
            ..Default::default()
        }));
    }

    #[test]
    #[should_panic(expected = "invariant 10")]
    fn parent_removed_before_child_panics() {
        let mut model = ClientModel::new();
        model.apply(&channel(ROOT_CHANNEL_ID, None));
        model.apply(&channel(1, Some(ROOT_CHANNEL_ID)));
        model.apply(&channel(2, Some(1)));
        model.apply(&ControlMessage::ChannelRemove(tcp::ChannelRemove {
            channel_id: 1,
        }));
    }

    #[test]
    #[should_panic(expected = "invariant 12")]
    fn retired_channel_id_reuse_panics() {
        let mut model = ClientModel::new();
        model.apply(&channel(ROOT_CHANNEL_ID, None));
        model.apply(&channel(1, Some(ROOT_CHANNEL_ID)));
        model.apply(&ControlMessage::ChannelRemove(tcp::ChannelRemove {
            channel_id: 1,
        }));
        model.apply(&channel(1, Some(ROOT_CHANNEL_ID)));
    }

    #[test]
    #[should_panic(expected = "invariant 12")]
    fn retired_session_id_reuse_panics() {
        let mut model = ClientModel::new();
        model.apply(&channel(ROOT_CHANNEL_ID, None));
        model.apply(&user(1, Some(ROOT_CHANNEL_ID)));
        model.apply(&ControlMessage::UserRemove(tcp::UserRemove {
            session: 1,
            ..Default::default()
        }));
        // Same id, another identity: the client's per-user local state would
        // follow the id onto a stranger.
        model.apply(&ControlMessage::UserState(tcp::UserState {
            session: Some(1),
            name: Some("someone else".to_string()),
            channel_id: Some(ROOT_CHANNEL_ID),
            ..Default::default()
        }));
    }

    #[test]
    fn a_hidden_user_may_become_visible_again_under_its_own_session() {
        let mut model = ClientModel::new();
        model.apply(&channel(ROOT_CHANNEL_ID, None));
        model.apply(&user(1, Some(ROOT_CHANNEL_ID)));
        model.apply(&ControlMessage::UserRemove(tcp::UserRemove {
            session: 1,
            ..Default::default()
        }));

        // A projection that stops showing a user and shows it again keeps its
        // session: it is the same person, not a recycled id.
        model.apply(&user(1, Some(ROOT_CHANNEL_ID)));
        assert_eq!(
            model.users.get(&1).map(|user| user.name.clone()),
            Some("user1".to_string())
        );
    }

    #[test]
    fn every_control_entity_reference_is_checked() {
        let invisible = 99;
        let cases = vec![
            (
                "ChannelRemove.channel_id",
                ControlMessage::ChannelRemove(tcp::ChannelRemove {
                    channel_id: invisible,
                }),
            ),
            (
                "ChannelState.parent",
                ControlMessage::ChannelState(tcp::ChannelState {
                    channel_id: Some(ROOT_CHANNEL_ID),
                    parent: Some(invisible),
                    ..Default::default()
                }),
            ),
            (
                "ChannelState.links",
                ControlMessage::ChannelState(tcp::ChannelState {
                    channel_id: Some(ROOT_CHANNEL_ID),
                    links: vec![invisible],
                    ..Default::default()
                }),
            ),
            (
                "ChannelState.links_add",
                ControlMessage::ChannelState(tcp::ChannelState {
                    channel_id: Some(ROOT_CHANNEL_ID),
                    links_add: vec![invisible],
                    ..Default::default()
                }),
            ),
            (
                "ChannelState.links_remove",
                ControlMessage::ChannelState(tcp::ChannelState {
                    channel_id: Some(ROOT_CHANNEL_ID),
                    links_remove: vec![invisible],
                    ..Default::default()
                }),
            ),
            (
                "UserState.actor",
                ControlMessage::UserState(tcp::UserState {
                    session: Some(1),
                    actor: Some(invisible),
                    ..Default::default()
                }),
            ),
            (
                "UserState.channel_id",
                ControlMessage::UserState(tcp::UserState {
                    session: Some(1),
                    channel_id: Some(invisible),
                    ..Default::default()
                }),
            ),
            (
                "UserState.listening_channel_add",
                ControlMessage::UserState(tcp::UserState {
                    session: Some(1),
                    listening_channel_add: vec![invisible],
                    ..Default::default()
                }),
            ),
            (
                "UserState.listening_channel_remove",
                ControlMessage::UserState(tcp::UserState {
                    session: Some(1),
                    listening_channel_remove: vec![invisible],
                    ..Default::default()
                }),
            ),
            (
                "UserState.VolumeAdjustment.listening_channel",
                ControlMessage::UserState(tcp::UserState {
                    session: Some(1),
                    listening_volume_adjustment: vec![tcp::user_state::VolumeAdjustment {
                        listening_channel: Some(invisible),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            ),
            (
                "UserRemove.session",
                ControlMessage::UserRemove(tcp::UserRemove {
                    session: invisible,
                    ..Default::default()
                }),
            ),
            (
                "UserRemove.actor",
                ControlMessage::UserRemove(tcp::UserRemove {
                    session: 1,
                    actor: Some(invisible),
                    ..Default::default()
                }),
            ),
            (
                "TextMessage.actor",
                ControlMessage::TextMessage(tcp::TextMessage {
                    actor: Some(invisible),
                    ..Default::default()
                }),
            ),
            (
                "TextMessage.session",
                ControlMessage::TextMessage(tcp::TextMessage {
                    session: vec![invisible],
                    ..Default::default()
                }),
            ),
            (
                "TextMessage.channel_id",
                ControlMessage::TextMessage(tcp::TextMessage {
                    channel_id: vec![invisible],
                    ..Default::default()
                }),
            ),
            (
                "TextMessage.tree_id",
                ControlMessage::TextMessage(tcp::TextMessage {
                    tree_id: vec![invisible],
                    ..Default::default()
                }),
            ),
            (
                "PermissionDenied.channel_id",
                ControlMessage::PermissionDenied(tcp::PermissionDenied {
                    channel_id: Some(invisible),
                    ..Default::default()
                }),
            ),
            (
                "PermissionDenied.session",
                ControlMessage::PermissionDenied(tcp::PermissionDenied {
                    session: Some(invisible),
                    ..Default::default()
                }),
            ),
            (
                "ACL.channel_id",
                ControlMessage::Acl(tcp::Acl {
                    channel_id: invisible,
                    ..Default::default()
                }),
            ),
            (
                "ContextAction.session",
                ControlMessage::ContextAction(tcp::ContextAction {
                    session: Some(invisible),
                    ..Default::default()
                }),
            ),
            (
                "ContextAction.channel_id",
                ControlMessage::ContextAction(tcp::ContextAction {
                    channel_id: Some(invisible),
                    ..Default::default()
                }),
            ),
            (
                "UserList.last_channel",
                ControlMessage::UserList(tcp::UserList {
                    users: vec![tcp::user_list::User {
                        last_channel: Some(invisible),
                        ..Default::default()
                    }],
                }),
            ),
            (
                "VoiceTarget.session",
                ControlMessage::VoiceTarget(tcp::VoiceTarget {
                    targets: vec![tcp::voice_target::Target {
                        session: vec![invisible],
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            ),
            (
                "VoiceTarget.channel_id",
                ControlMessage::VoiceTarget(tcp::VoiceTarget {
                    targets: vec![tcp::voice_target::Target {
                        channel_id: Some(invisible),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            ),
            (
                "PermissionQuery.channel_id",
                ControlMessage::PermissionQuery(tcp::PermissionQuery {
                    channel_id: Some(invisible),
                    ..Default::default()
                }),
            ),
            (
                "UserStats.session",
                ControlMessage::UserStats(tcp::UserStats {
                    session: Some(invisible),
                    ..Default::default()
                }),
            ),
            (
                "RequestBlob.session_texture",
                ControlMessage::RequestBlob(tcp::RequestBlob {
                    session_texture: vec![invisible],
                    ..Default::default()
                }),
            ),
            (
                "RequestBlob.session_comment",
                ControlMessage::RequestBlob(tcp::RequestBlob {
                    session_comment: vec![invisible],
                    ..Default::default()
                }),
            ),
            (
                "RequestBlob.channel_description",
                ControlMessage::RequestBlob(tcp::RequestBlob {
                    channel_description: vec![invisible],
                    ..Default::default()
                }),
            ),
            (
                "PluginDataTransmission.sender_session",
                ControlMessage::PluginDataTransmission(tcp::PluginDataTransmission {
                    sender_session: Some(invisible),
                    ..Default::default()
                }),
            ),
            (
                "PluginDataTransmission.receiver_sessions",
                ControlMessage::PluginDataTransmission(tcp::PluginDataTransmission {
                    receiver_sessions: vec![invisible],
                    ..Default::default()
                }),
            ),
        ];

        for (label, message) in cases {
            rejects_invisible_reference(label, message);
        }
    }

    #[test]
    #[should_panic(expected = "Audio.sender_session")]
    fn udp_audio_from_invisible_sender_panics() {
        synced_model().apply_audio(&udp::Audio {
            sender_session: 99,
            ..Default::default()
        });
    }

    #[test]
    #[should_panic(expected = "Audio.sender_session")]
    fn tunneled_audio_from_invisible_sender_panics() {
        let bytes = voxloom_protocol::encode_udp(&UdpMessage::Audio(udp::Audio {
            sender_session: 99,
            ..Default::default()
        }));
        synced_model().apply(&ControlMessage::UdpTunnel(bytes));
    }
}
