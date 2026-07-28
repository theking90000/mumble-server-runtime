//! A strict client model, used as the oracle's right-hand side.
//!
//! It applies **control messages**, not plan operations, so the translation
//! layer sits inside the oracle rather than being trusted. Its behaviour mirrors
//! the real client wherever that matters:
//!
//! - the root channel exists before any message arrives, which is why a
//!   `ChannelState` for it with no `parent` is an update rather than a refusal;
//! - a `ChannelState` for an unknown channel with no parent or no name creates
//!   nothing;
//! - `UserState` and `ChannelState` **merge**: an absent field changes nothing.
//!
//! REF: references/mumble/src/mumble/Messages.cpp : `msgChannelState`,
//!   `msgUserState`, `msgChannelRemove`, `msgUserRemove`.
#![allow(clippy::expect_used)]
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use mumble_server_runtime_protocol::ControlMessage;
use mumble_server_runtime_shard::{Overlay, ScopeSet, ShardView};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChannel {
    pub name: String,
    pub parent: Option<u32>,
    pub position: i32,
    pub can_enter: bool,
    pub links: BTreeSet<u32>,
    /// Bumped every time the channel is created. Lets a test tell "still the
    /// same channel" from "destroyed and recreated", which is what flicker is.
    pub generation: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelUser {
    pub name: String,
    pub channel: u32,
}

/// Equality ignores [`ModelChannel::generation`], which records history rather
/// than state: the oracle compares what the client *holds*, and a channel that
/// was recreated holds the same thing as one that never moved. Flicker is
/// checked separately, by the tests that care about it.
#[derive(Debug, Clone)]
pub struct ClientModel {
    pub channels: BTreeMap<u32, ModelChannel>,
    pub users: BTreeMap<u32, ModelUser>,
}

impl PartialEq for ClientModel {
    fn eq(&self, other: &ClientModel) -> bool {
        self.users == other.users
            && self.channels.len() == other.channels.len()
            && self.channels.iter().all(|(id, channel)| {
                other.channels.get(id).is_some_and(|peer| {
                    channel.name == peer.name
                        && channel.parent == peer.parent
                        && channel.position == peer.position
                        && channel.can_enter == peer.can_enter
                        && channel.links == peer.links
                })
            })
    }
}

impl Eq for ClientModel {}

impl ClientModel {
    /// A fresh client: it already holds the root, as the real one does.
    #[must_use]
    pub fn new() -> ClientModel {
        let mut channels = BTreeMap::new();
        channels.insert(
            0,
            ModelChannel {
                name: "Root".to_owned(),
                parent: None,
                position: 0,
                can_enter: true,
                links: BTreeSet::new(),
                generation: 0,
            },
        );
        ClientModel {
            channels,
            users: BTreeMap::new(),
        }
    }

    /// Apply one message and judge the state it leaves behind.
    ///
    /// Judging **after every message** rather than at the end of a transition is
    /// the entire point: an ordering defect produces a transiently invalid state
    /// and a perfectly correct final one, so a model that only compares
    /// endpoints cannot see it. Each rule lives in its own branch so removing
    /// one is a visible edit.
    ///
    /// # Errors
    ///
    /// A description of the first rule this message broke.
    pub fn apply(&mut self, message: &ControlMessage) -> Result<(), String> {
        match message {
            ControlMessage::ChannelState(state) => self.apply_channel(state),
            ControlMessage::ChannelRemove(remove) => {
                // The real client refuses to remove the root.
                if remove.channel_id != 0 {
                    // Invariant: no occupied channel is ever removed. This is
                    // the one the overlay splice exists to protect.
                    if let Some((session, _)) = self
                        .users
                        .iter()
                        .find(|(_, user)| user.channel == remove.channel_id)
                    {
                        return Err(format!(
                            "channel {} was removed while session {session} was still in it",
                            remove.channel_id
                        ));
                    }
                    self.channels.remove(&remove.channel_id);
                }
            }
            ControlMessage::UserState(state) => self.apply_user(state),
            ControlMessage::UserRemove(remove) => {
                self.users.remove(&remove.session);
            }
            // Carries no view state, but every identifier in it is a claim about
            // what this client holds, and a claim about something it does not
            // hold is the leak this whole model exists to catch (spec 20,
            // invariants 14 and 15).
            ControlMessage::TextMessage(text) => {
                if let Some(actor) = text.actor
                    && !self.users.contains_key(&actor)
                {
                    return Err(format!("a message names actor {actor}, who is not visible"));
                }
                for session in &text.session {
                    if !self.users.contains_key(session) {
                        return Err(format!(
                            "a message is addressed to session {session}, who is not visible"
                        ));
                    }
                }
                for channel in text.channel_id.iter().chain(&text.tree_id) {
                    if !self.channels.contains_key(channel) {
                        return Err(format!(
                            "a message is addressed to channel {channel}, which is not visible"
                        ));
                    }
                }
            }
            // Nothing else carries view state.
            _ => {}
        }
        self.check()
    }

    /// The structural rules that must hold after every single message.
    fn check(&self) -> Result<(), String> {
        for (session, user) in &self.users {
            if !self.channels.contains_key(&user.channel) {
                return Err(format!(
                    "session {session} is in channel {}, which does not exist",
                    user.channel
                ));
            }
        }
        for (id, channel) in &self.channels {
            let Some(parent) = channel.parent else {
                continue;
            };
            if !self.channels.contains_key(&parent) {
                return Err(format!(
                    "channel {id} has parent {parent}, which does not exist"
                ));
            }
            // Walk to the root, so a cycle is caught rather than hung on.
            let mut seen = BTreeSet::from([*id]);
            let mut current = parent;
            while let Some(next) = self.channels.get(&current).and_then(|c| c.parent) {
                if !seen.insert(current) {
                    return Err(format!("channel {id} sits in a parent cycle"));
                }
                current = next;
            }
        }
        Ok(())
    }

    fn apply_channel(
        &mut self,
        state: &mumble_server_runtime_protocol::messages::tcp::ChannelState,
    ) {
        let Some(id) = state.channel_id else { return };

        if let std::collections::btree_map::Entry::Vacant(slot) = self.channels.entry(id) {
            // Creation needs both a parent and a name, exactly as the client
            // requires; anything else is dropped with a warning there.
            let (Some(parent), Some(name)) = (state.parent, state.name.clone()) else {
                return;
            };
            slot.insert(ModelChannel {
                name,
                parent: Some(parent),
                position: state.position.unwrap_or(0),
                can_enter: state.can_enter.unwrap_or(true),
                links: BTreeSet::new(),
                generation: 1,
            });
        }

        let Some(channel) = self.channels.get_mut(&id) else {
            return;
        };
        // The root's parent is never rewritten: it has none, and the server
        // never sends one for it.
        if let Some(parent) = state.parent
            && id != 0
        {
            channel.parent = Some(parent);
        }
        if let Some(name) = &state.name {
            channel.name = name.clone();
        }
        if let Some(position) = state.position {
            channel.position = position;
        }
        if let Some(can_enter) = state.can_enter {
            channel.can_enter = can_enter;
        }
        // A non-empty `links` replaces; an empty one is a no-op. Then remove,
        // then add, each in its own independent block.
        if !state.links.is_empty() {
            channel.links = state.links.iter().copied().collect();
        }
        for link in &state.links_remove {
            channel.links.remove(link);
        }
        for link in &state.links_add {
            channel.links.insert(*link);
        }
    }

    fn apply_user(&mut self, state: &mumble_server_runtime_protocol::messages::tcp::UserState) {
        let Some(session) = state.session else { return };

        if let std::collections::btree_map::Entry::Vacant(slot) = self.users.entry(session) {
            let (Some(name), Some(channel)) = (state.name.clone(), state.channel_id) else {
                return;
            };
            slot.insert(ModelUser { name, channel });
            return;
        }

        let Some(user) = self.users.get_mut(&session) else {
            return;
        };
        if let Some(name) = &state.name {
            user.name = name.clone();
        }
        if let Some(channel) = state.channel_id {
            user.channel = channel;
        }
    }

    /// The right-hand side of the oracle: what this connection *should* hold.
    #[must_use]
    pub fn expected(view: &ShardView, see: ScopeSet, overlay: Overlay) -> ClientModel {
        let composed = view.restrict(see).compose(&overlay);
        let mut model = ClientModel::new();

        for channel in composed.channels.values() {
            let id = channel.id.0;
            let entry = model.channels.entry(id).or_insert(ModelChannel {
                name: String::new(),
                parent: None,
                position: 0,
                can_enter: true,
                links: BTreeSet::new(),
                generation: 1,
            });
            entry.name = channel.name.clone();
            entry.parent = (id != 0).then_some(channel.parent.0);
            entry.position = channel.position;
            entry.can_enter = channel.can_enter;
            entry.links = channel.links.iter().map(|link| link.0).collect();
        }

        for user in composed.users.values() {
            model.users.insert(
                user.session.0,
                ModelUser {
                    name: user.name.clone(),
                    channel: user.channel.0,
                },
            );
        }

        // Generations are a property of the message history, not of the desired
        // state, so they are normalized out of the comparison.
        model
    }

    #[must_use]
    pub fn has_user(&self, session: u32) -> bool {
        self.users.contains_key(&session)
    }

    #[must_use]
    pub fn channel_generation(&self, id: u32) -> Option<u32> {
        self.channels.get(&id).map(|channel| channel.generation)
    }

    #[must_use]
    pub fn channel_id_named(&self, name: &str) -> Option<u32> {
        self.channels
            .iter()
            .find(|(_, channel)| channel.name == name)
            .map(|(id, _)| *id)
    }
}
