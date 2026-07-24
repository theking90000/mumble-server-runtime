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

use std::collections::BTreeMap;

use voxloom_protocol::ControlMessage;
use voxloom_protocol::messages::tcp;

/// The root channel is always id 0 (spec §11.2, §20 invariant 1).
pub const ROOT_CHANNEL_ID: u32 = 0;

/// A channel as the client models it.
#[derive(Debug, Clone)]
pub struct ModelChannel {
    pub id: u32,
    /// Parent channel id; `None` for the root.
    pub parent: Option<u32>,
    pub name: String,
}

/// A user as the client models it.
#[derive(Debug, Clone)]
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
}

impl ClientModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one server->client control message, enforcing every §20 invariant it
    /// touches. Panics on any violation (the model's contract, §26.6).
    pub fn apply(&mut self, message: &ControlMessage) {
        match message {
            ControlMessage::ChannelState(cs) => self.apply_channel_state(cs),
            ControlMessage::ChannelRemove(cr) => self.apply_channel_remove(cr),
            ControlMessage::UserState(us) => self.apply_user_state(us),
            ControlMessage::UserRemove(ur) => self.apply_user_remove(ur),
            ControlMessage::ServerSync(sync) => self.apply_server_sync(sync),
            // Other messages (Version, CryptSetup, CodecVersion, ServerConfig,
            // Ping, PermissionQuery...) carry no §20 structural invariant for the
            // client model to enforce here; they are accepted.
            _ => {}
        }
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
            // Invariant 3: a new child channel must reference an already-visible
            // parent (the root, id 0, is the only channel allowed to have none).
            self.check_parent_visible(id, cs.parent);
        }

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
        // Invariant 8: an occupied channel is never removed.
        self.check_channel_empty(id);
        self.channels.remove(&id);
    }

    fn apply_user_state(&mut self, us: &tcp::UserState) {
        let session = match us.session {
            Some(session) => session,
            None => panic!("§20: UserState without a session id"),
        };

        // Invariant 15: an actor, if named, must be a visible session.
        if let Some(actor) = us.actor {
            self.check_actor_visible(actor);
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
        // Removing our own session is a disconnect; removing another is a normal
        // presence update. Either way, drop it from the view.
        self.users.remove(&ur.session);
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

    /// Invariant 15: a referenced actor session must be visible.
    fn check_actor_visible(&self, actor: u32) {
        if !self.users.contains_key(&actor) {
            panic!("§20 invariant 15: message references invisible actor session {actor}");
        }
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
    #[should_panic(expected = "invariant 15")]
    fn invisible_actor_panics() {
        let mut model = ClientModel::new();
        model.apply(&channel(0, None));
        model.apply(&ControlMessage::UserState(tcp::UserState {
            session: Some(1),
            actor: Some(42), // session 42 is not visible
            channel_id: Some(0),
            ..Default::default()
        }));
    }
}
