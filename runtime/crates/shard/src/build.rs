//! The render builder: the only way a flavor produces a view.
//!
//! # Why an incoherent view is not expressible
//!
//! [`ShardBuilder::channel`] demands a parent and [`ShardBuilder::user`] demands
//! a channel; in both cases the scope is *derived* from the parent's through
//! [`Narrow`], which can only extend. There is no free scope parameter anywhere.
//! That is what turns the closure property of [`crate::scope`] from something to
//! validate into something to rely on: a user is always at or below its
//! channel's scope, so any observer who sees the user also sees the channel.
//!
//! The one thing that does not follow the hierarchy is a link between channels,
//! and it is therefore the one structural check in the model.
//!
//! # What `finish` still has to check
//!
//! Three properties are not structural, and each one is a real failure mode:
//!
//! - **Shared xor private** (guide 3.4). Without it the shared view would say
//!   "admin in /g7/staff" while an overlay says "admin in A's channel", and the
//!   next shared delta would move the admin without the overlay reasserting
//!   itself. The client would drift silently, which is the worst class of bug
//!   this design can produce.
//! - **An overlay only references what its connection can already see**
//!   (guide 6.6). One lookup per overlay element forbids, in one stroke, placing
//!   someone in a channel that is about to vanish, in a channel that connection
//!   cannot see, or referencing a session that does not exist.
//! - **A receiver sees its sender** (guide 1.2). This is not a design choice: the
//!   Mumble client discards audio whose sender session it does not know, so an
//!   edge into a blind receiver is silence with extra steps.
//!
//! REF: docs/design/guide-implementation.md 3

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::ids::{
    ActionKey, ChannelId, ChannelKey, ConnectionId, Exhausted, Occupant, SessionId, ShardId,
    SharedIds,
};
use crate::routing::{AudioRelation, DomainId};
use crate::scope::{Scope, ScopeSet};
use crate::view::{Action, Actions, Channel, On, Overlay, ShardView, User, UserFlags};

/// How many context actions one connection may be offered in a single render.
///
/// A bound rather than a taste: the whole turn goes into the connection's queue
/// in one piece, and a queue that refuses an oversized batch closes the
/// connection. Refusing the render is the failure that stays inside the flavor's
/// own bug.
pub const MAX_ACTIONS: usize = 64;

/// How a child's scope relates to its parent's.
///
/// There is no "widen" variant, and that absence is the whole safety argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Narrow {
    /// Same scope as the parent.
    Same,
    /// The parent's scope, extended by one segment.
    Into(u32),
}

/// A handle to a channel placed in this render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelRef {
    id: ChannelId,
    scope: Scope,
}

impl ChannelRef {
    #[must_use]
    pub fn id(self) -> ChannelId {
        self.id
    }

    #[must_use]
    pub fn scope(self) -> Scope {
        self.scope
    }
}

/// A handle to a user placed in this render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserRef {
    session: SessionId,
}

impl UserRef {
    #[must_use]
    pub fn session(self) -> SessionId {
        self.session
    }
}

/// Why a render was refused.
///
/// A refused render keeps the previous view and does not close any connection
/// (guide 11.7): the committed view is still correct, and reconnecting would
/// only reproduce the same broken render.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BuildError {
    #[error("the render never called `root`, so nothing anchors its channels")]
    MissingRoot,

    #[error("`root` was called twice; a shard renders one tree")]
    DuplicateRoot,

    #[error("channel key {0:?} was rendered twice in one turn")]
    DuplicateChannelKey(ChannelKey),

    #[error("occupant {0:?} was rendered twice in one turn")]
    DuplicateOccupant(Occupant),

    #[error(
        "narrowing past the maximum scope depth: {context}. Widening instead would hand this \
         element its parent's visibility"
    )]
    ScopeTooDeep { context: String },

    #[error(transparent)]
    Exhausted(#[from] Exhausted),

    #[error(
        "channel {a:?} is linked to {b:?}, whose scope is not comparable: a link to a channel the \
         viewer may not see has no meaning"
    )]
    LinkAcrossScopes { a: ChannelId, b: ChannelId },

    #[error(
        "session {session:?} is in the shared view and in connection {connection:?}'s overlay; an \
         element is shared xor private, never merged"
    )]
    SharedAndPrivateUser {
        session: SessionId,
        connection: ConnectionId,
    },

    #[error(
        "channel {channel:?} is in the shared view and in connection {connection:?}'s overlay; an \
         element is shared xor private, never merged"
    )]
    SharedAndPrivateChannel {
        channel: ChannelId,
        connection: ConnectionId,
    },

    #[error(
        "connection {connection:?}'s overlay places a user in channel {channel:?}, which is not in \
         the shared view after this render nor in its own overlay"
    )]
    OverlayChannelMissing {
        connection: ConnectionId,
        channel: ChannelId,
    },

    #[error(
        "connection {connection:?}'s overlay places a user in channel {channel:?}, which that \
         connection cannot see"
    )]
    OverlayChannelInvisible {
        connection: ConnectionId,
        channel: ChannelId,
    },

    #[error(
        "connection {connection:?} is offered more than {MAX_ACTIONS} context actions, which would \
         not fit in one transition"
    )]
    TooManyActions { connection: ConnectionId },

    #[error(
        "audio edge {sender:?} -> {receiver:?} would be discarded: the receiver cannot see the \
         sender, and the client drops audio from a session it does not know"
    )]
    ReceiverCannotSeeSender {
        sender: ConnectionId,
        receiver: ConnectionId,
    },
}

/// Everything one render produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub view: ShardView,
    pub overlays: BTreeMap<ConnectionId, Overlay>,
    /// What each connection is offered. Never journalled, for the same reason an
    /// overlay is not: it is recomputed per connection every turn.
    pub actions: BTreeMap<ConnectionId, Actions>,
    pub audio: AudioRelation,
}

/// The uniform constructor handed to [`crate::shard::ShardLogic::render`].
///
/// Every method that can fail records the refusal and carries on rather than
/// returning a `Result` the flavor would have to thread through its whole
/// render. The refusal is not lost: [`ShardBuilder::finish`] returns it and the
/// render is discarded whole. Handles returned after a refusal are meaningless,
/// which is harmless precisely because nothing built on them will be published.
pub struct ShardBuilder<'a> {
    ids: &'a SharedIds,
    /// Namespaces this render's channel keys. Two shards may use the same key
    /// for two different channels, and they must not collide on the wire.
    shard: ShardId,
    connections: &'a [ConnectionId],
    view: ShardView,
    overlays: BTreeMap<ConnectionId, Overlay>,
    actions: BTreeMap<ConnectionId, Actions>,
    audio: AudioRelation,
    keys_seen: BTreeSet<ChannelKey>,
    occupants_seen: BTreeSet<Occupant>,
    rooted: bool,
    /// First refusal wins. Listing the rest would buy nothing: the render is
    /// discarded whole, so a flavor fixes them one at a time regardless.
    error: Option<BuildError>,
}

impl<'a> ShardBuilder<'a> {
    #[must_use]
    pub fn new(
        ids: &'a SharedIds,
        shard: ShardId,
        connections: &'a [ConnectionId],
    ) -> ShardBuilder<'a> {
        ShardBuilder {
            ids,
            shard,
            connections,
            view: ShardView::empty(),
            overlays: BTreeMap::new(),
            actions: BTreeMap::new(),
            audio: AudioRelation::default(),
            keys_seen: BTreeSet::new(),
            occupants_seen: BTreeSet::new(),
            rooted: false,
            error: None,
        }
    }

    /// The connections attached to this shard, for overlay loops.
    #[must_use]
    pub fn connections(&self) -> &[ConnectionId] {
        self.connections
    }

    /// The root of this shard's tree: what every observer sees.
    pub fn root(&mut self, name: &str) -> ChannelRef {
        if self.rooted {
            self.fail(BuildError::DuplicateRoot);
            return ChannelRef {
                id: ChannelId::ROOT,
                scope: Scope::ROOT,
            };
        }
        self.rooted = true;

        self.view.channels.insert(
            ChannelId::ROOT,
            Channel {
                key: ChannelKey::ROOT,
                id: ChannelId::ROOT,
                parent: ChannelId::ROOT,
                scope: Scope::ROOT,
                name: name.to_owned(),
                position: 0,
                can_enter: true,
                can_text: true,
                links: BTreeSet::new(),
            },
        );
        ChannelRef {
            id: ChannelId::ROOT,
            scope: Scope::ROOT,
        }
    }

    /// A child channel. `narrow` can only extend the parent's scope.
    pub fn channel(
        &mut self,
        parent: ChannelRef,
        key: ChannelKey,
        name: &str,
        narrow: Narrow,
    ) -> ChannelRef {
        let Some(scope) = self.narrowed(parent.scope, narrow, || format!("channel {name:?}"))
        else {
            return parent;
        };
        if !self.keys_seen.insert(key) {
            self.fail(BuildError::DuplicateChannelKey(key));
            return parent;
        }
        let id = match self.ids.channel(self.shard, key) {
            Ok(id) => id,
            Err(exhausted) => {
                self.fail(BuildError::Exhausted(exhausted));
                return parent;
            }
        };

        self.view.channels.insert(
            id,
            Channel {
                key,
                id,
                parent: parent.id,
                scope,
                name: name.to_owned(),
                position: 0,
                can_enter: true,
                can_text: true,
                links: BTreeSet::new(),
            },
        );
        ChannelRef { id, scope }
    }

    /// A user in a channel. `narrow` can only extend the channel's scope.
    pub fn user(
        &mut self,
        channel: ChannelRef,
        who: Occupant,
        name: &str,
        narrow: Narrow,
    ) -> UserRef {
        let scope = self
            .narrowed(channel.scope, narrow, || format!("user {name:?}"))
            .unwrap_or(channel.scope);
        let session = match self.ids.session(who) {
            Ok(session) => session,
            Err(exhausted) => {
                self.fail(BuildError::Exhausted(exhausted));
                // The scope is already refused; any session works for the
                // handle since finish will discard the whole render.
                SessionId(0)
            }
        };
        if !self.occupants_seen.insert(who) {
            self.fail(BuildError::DuplicateOccupant(who));
            return UserRef { session };
        }

        self.view.users.insert(
            session,
            User {
                occupant: who,
                session,
                channel: channel.id,
                scope,
                name: name.to_owned(),
                flags: UserFlags::default(),
            },
        );
        UserRef { session }
    }

    pub fn channel_position(&mut self, channel: ChannelRef, position: i32) {
        if let Some(entry) = self.view.channels.get_mut(&channel.id) {
            entry.position = position;
        }
    }

    pub fn channel_can_enter(&mut self, channel: ChannelRef, yes: bool) {
        if let Some(entry) = self.view.channels.get_mut(&channel.id) {
            entry.can_enter = yes;
        }
    }

    /// Whether text may be addressed to this channel.
    ///
    /// Declaring it false greys the client's chat box out for that channel
    /// instead of letting the user type into something that answers
    /// `PermissionDenied`, and the shard refuses a message aimed there anyway.
    pub fn channel_can_text(&mut self, channel: ChannelRef, yes: bool) {
        if let Some(entry) = self.view.channels.get_mut(&channel.id) {
            entry.can_text = yes;
        }
    }

    /// Link two channels, symmetrically.
    ///
    /// The only structural check in the model, because a link is the one
    /// relation that does not follow the parent hierarchy: a link pointing at a
    /// channel the viewer may not see has no meaning.
    pub fn channel_link(&mut self, a: ChannelRef, b: ChannelRef) {
        if !a.scope.comparable(b.scope) {
            self.fail(BuildError::LinkAcrossScopes { a: a.id, b: b.id });
            return;
        }
        if let Some(entry) = self.view.channels.get_mut(&a.id) {
            entry.links.insert(b.id);
        }
        if let Some(entry) = self.view.channels.get_mut(&b.id) {
            entry.links.insert(a.id);
        }
    }

    pub fn user_flags(&mut self, user: UserRef, flags: UserFlags) {
        if let Some(entry) = self.view.users.get_mut(&user.session) {
            entry.flags = flags;
        }
    }

    /// Elements visible to this connection only.
    pub fn private(&mut self, connection: ConnectionId, build: impl FnOnce(&mut PrivateBuilder)) {
        let mut private = PrivateBuilder {
            ids: self.ids,
            shard: self.shard,
            connection,
            overlay: self.overlays.entry(connection).or_default(),
            actions: self.actions.entry(connection).or_default(),
            error: &mut self.error,
        };
        build(&mut private);
    }

    /// A symmetric group: everyone in the domain hears everyone else.
    pub fn audio_domain(&mut self, domain: DomainId, members: &[ConnectionId]) {
        self.audio.domain(domain, members);
    }

    /// A one-way exception: hears the domain, is not heard by it.
    pub fn audio_listen(&mut self, listener: ConnectionId, domain: DomainId) {
        self.audio.listen(listener, domain);
    }

    /// A single directed edge, for full generality.
    pub fn audio_edge(&mut self, sender: ConnectionId, receiver: ConnectionId) {
        self.audio.edge(sender, receiver);
    }

    /// Close the render, checking the three non-structural properties.
    ///
    /// `observations` is what each attached connection observes of the shared
    /// view; it is needed for the overlay and audio checks, both of which are
    /// statements about who can see what.
    ///
    /// # Errors
    ///
    /// The first [`BuildError`] recorded during the render, or the first one the
    /// closing checks find.
    pub fn finish(
        self,
        observations: &BTreeMap<ConnectionId, ScopeSet>,
    ) -> Result<Rendered, BuildError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if !self.rooted {
            return Err(BuildError::MissingRoot);
        }

        check_shared_xor_private(&self.view, &self.overlays)?;
        check_overlay_references(&self.view, &self.overlays, observations)?;
        check_receivers_see_senders(&self.view, &self.overlays, &self.audio, observations)?;

        Ok(Rendered {
            view: self.view,
            overlays: self.overlays,
            actions: self.actions,
            audio: self.audio,
        })
    }

    /// Apply a [`Narrow`], recording a refusal past the depth bound.
    fn narrowed(
        &mut self,
        parent: Scope,
        narrow: Narrow,
        context: impl FnOnce() -> String,
    ) -> Option<Scope> {
        match narrow {
            Narrow::Same => Some(parent),
            Narrow::Into(segment) => match parent.child(segment) {
                Some(scope) => Some(scope),
                None => {
                    self.fail(BuildError::ScopeTooDeep { context: context() });
                    None
                }
            },
        }
    }

    fn fail(&mut self, error: BuildError) {
        self.error.get_or_insert(error);
    }
}

/// The constructor for one connection's private elements.
pub struct PrivateBuilder<'a> {
    ids: &'a SharedIds,
    shard: ShardId,
    connection: ConnectionId,
    overlay: &'a mut Overlay,
    actions: &'a mut Actions,
    error: &'a mut Option<BuildError>,
}

impl PrivateBuilder<'_> {
    /// Place a user in a channel, visible to this connection only.
    ///
    /// The channel is one this connection can already see - which
    /// [`ShardBuilder::finish`] verifies rather than assumes.
    pub fn user_in(&mut self, channel: ChannelRef, who: Occupant, name: &str) -> UserRef {
        let session = match self.ids.session(who) {
            Ok(session) => session,
            Err(exhausted) => {
                self.error.get_or_insert(BuildError::Exhausted(exhausted));
                SessionId(0)
            }
        };

        self.overlay.users.insert(
            session,
            User {
                occupant: who,
                session,
                channel: channel.id,
                // Private elements are never scope-filtered; carrying the
                // channel's scope keeps the type uniform and lets the closing
                // checks compare without a special case.
                scope: channel.scope,
                name: name.to_owned(),
                flags: UserFlags::default(),
            },
        );
        UserRef { session }
    }

    /// A channel visible to this connection only.
    pub fn channel(&mut self, parent: ChannelRef, key: ChannelKey, name: &str) -> ChannelRef {
        let id = match self.ids.channel(self.shard, key) {
            Ok(id) => id,
            Err(exhausted) => {
                self.error.get_or_insert(BuildError::Exhausted(exhausted));
                return parent;
            }
        };

        self.overlay.channels.insert(
            id,
            Channel {
                key,
                id,
                parent: parent.id,
                scope: parent.scope,
                name: name.to_owned(),
                position: 0,
                can_enter: true,
                can_text: true,
                links: BTreeSet::new(),
            },
        );
        ChannelRef {
            id,
            scope: parent.scope,
        }
    }

    pub fn user_flags(&mut self, user: UserRef, flags: UserFlags) {
        if let Some(entry) = self.overlay.users.get_mut(&user.session) {
            entry.flags = flags;
        }
    }

    /// Offer this connection a context action: a button that is not a channel to
    /// double-click.
    ///
    /// Declared like everything else, and withdrawn by simply not declaring it
    /// again. The flavor never emits a message; the difference with what this
    /// connection was already offered is what travels.
    ///
    /// Offering the same key twice in one render is the last call winning, which
    /// is the same rule a flavor already gets from rendering a user twice.
    pub fn action(&mut self, key: ActionKey, text: &str, on: On) {
        if self.actions.len() >= MAX_ACTIONS && !self.actions.contains_key(&key) {
            self.error.get_or_insert(BuildError::TooManyActions {
                connection: self.connection,
            });
            return;
        }
        self.actions.insert(
            key,
            Action {
                key,
                text: text.to_owned(),
                on,
            },
        );
    }
}

/// Guide 3.4: nothing is both shared and private.
fn check_shared_xor_private(
    view: &ShardView,
    overlays: &BTreeMap<ConnectionId, Overlay>,
) -> Result<(), BuildError> {
    for (connection, overlay) in overlays {
        if let Some(session) = overlay
            .users
            .keys()
            .find(|session| view.users.contains_key(session))
        {
            return Err(BuildError::SharedAndPrivateUser {
                session: *session,
                connection: *connection,
            });
        }
        if let Some(channel) = overlay
            .channels
            .keys()
            .find(|channel| view.channels.contains_key(channel))
        {
            return Err(BuildError::SharedAndPrivateChannel {
                channel: *channel,
                connection: *connection,
            });
        }
    }
    Ok(())
}

/// Guide 6.6: an overlay only references what its connection can already see.
fn check_overlay_references(
    view: &ShardView,
    overlays: &BTreeMap<ConnectionId, Overlay>,
    observations: &BTreeMap<ConnectionId, ScopeSet>,
) -> Result<(), BuildError> {
    for (connection, overlay) in overlays {
        let see = observations
            .get(connection)
            .copied()
            .unwrap_or(ScopeSet::NONE);

        for user in overlay.users.values() {
            if overlay.channels.contains_key(&user.channel) {
                continue;
            }
            let Some(channel) = view.channels.get(&user.channel) else {
                return Err(BuildError::OverlayChannelMissing {
                    connection: *connection,
                    channel: user.channel,
                });
            };
            if !see.sees(channel.scope) {
                return Err(BuildError::OverlayChannelInvisible {
                    connection: *connection,
                    channel: user.channel,
                });
            }
        }

        for channel in overlay.channels.values() {
            if overlay.channels.contains_key(&channel.parent) {
                continue;
            }
            let Some(parent) = view.channels.get(&channel.parent) else {
                return Err(BuildError::OverlayChannelMissing {
                    connection: *connection,
                    channel: channel.parent,
                });
            };
            if !see.sees(parent.scope) {
                return Err(BuildError::OverlayChannelInvisible {
                    connection: *connection,
                    channel: channel.parent,
                });
            }
        }
    }
    Ok(())
}

/// Guide 1.2: every audio edge lands on a receiver that can see the sender.
///
/// Not a design choice. The Mumble client looks the sender session up before
/// buffering a voice frame and discards the frame when the lookup fails.
///
/// REF: runtime/references/mumble/src/mumble/ServerHandler.cpp : `handleVoicePacket`
///   buffers only when `ClientUser::get(audioData.senderSession)` succeeds.
/// # Why this is not a loop over pairs
///
/// The obvious form - resolve the relation to its edges and test each one - is
/// quadratic in a domain's size, and it runs on **every** render even when
/// nothing about the audio changed. At 500 connections that alone cost more
/// than the entire rest of a turn.
///
/// The same statement holds with far less work: within a domain, what a
/// receiver must see is not each sender but each *distinct scope* the senders
/// occupy, and there are usually one or two of those. Counting members per
/// scope keeps the "everyone but me" exclusion exact without ever forming a
/// pair.
fn check_receivers_see_senders(
    view: &ShardView,
    overlays: &BTreeMap<ConnectionId, Overlay>,
    audio: &AudioRelation,
    observations: &BTreeMap<ConnectionId, ScopeSet>,
) -> Result<(), BuildError> {
    let shared_scope: BTreeMap<ConnectionId, Scope> = view
        .users
        .values()
        .filter_map(|user| user.occupant.connection().map(|c| (c, user.scope)))
        .collect();

    // Who each connection can see privately, resolved once instead of per edge.
    let privately: BTreeMap<ConnectionId, BTreeSet<ConnectionId>> = overlays
        .iter()
        .map(|(connection, overlay)| {
            let visible = overlay
                .users
                .values()
                .filter_map(|user| user.occupant.connection())
                .collect();
            (*connection, visible)
        })
        .collect();

    for (_, members) in audio.domains() {
        check_group(
            members.iter().copied(),
            members,
            &shared_scope,
            &privately,
            observations,
        )?;
    }

    for (listener, members) in audio.listeners() {
        check_group(
            std::iter::once(listener),
            members,
            &shared_scope,
            &privately,
            observations,
        )?;
    }

    for (sender, receiver) in audio.explicit_edges() {
        if !can_see(receiver, sender, &shared_scope, &privately, observations) {
            return Err(BuildError::ReceiverCannotSeeSender { sender, receiver });
        }
    }
    Ok(())
}

/// Check that every receiver can see every sender other than itself.
fn check_group(
    receivers: impl Iterator<Item = ConnectionId>,
    senders: &BTreeSet<ConnectionId>,
    shared_scope: &BTreeMap<ConnectionId, Scope>,
    privately: &BTreeMap<ConnectionId, BTreeSet<ConnectionId>>,
    observations: &BTreeMap<ConnectionId, ScopeSet>,
) -> Result<(), BuildError> {
    // Senders grouped by the scope they occupy, plus the ones the shared view
    // does not carry at all, which have to be checked one by one.
    let mut per_scope: BTreeMap<Scope, usize> = BTreeMap::new();
    let mut hidden: Vec<ConnectionId> = Vec::new();
    for sender in senders {
        match shared_scope.get(sender) {
            Some(scope) => *per_scope.entry(*scope).or_default() += 1,
            None => hidden.push(*sender),
        }
    }

    for receiver in receivers {
        let see = observations
            .get(&receiver)
            .copied()
            .unwrap_or(ScopeSet::NONE);
        let own = shared_scope.get(&receiver).copied();

        for (scope, count) in &per_scope {
            // A receiver never sends to itself, so its own presence at this
            // scope does not oblige it to see the scope.
            let others = if own == Some(*scope) {
                count.saturating_sub(1)
            } else {
                *count
            };
            if others > 0 && !see.sees(*scope) {
                return Err(BuildError::ReceiverCannotSeeSender {
                    sender: representative(senders, shared_scope, *scope, receiver),
                    receiver,
                });
            }
        }

        for sender in &hidden {
            if *sender != receiver
                && !privately
                    .get(&receiver)
                    .is_some_and(|visible| visible.contains(sender))
            {
                return Err(BuildError::ReceiverCannotSeeSender {
                    sender: *sender,
                    receiver,
                });
            }
        }
    }
    Ok(())
}

/// A sender at `scope` other than `receiver`, so the error names something the
/// reader can go and look at. Only walked on the failure path.
fn representative(
    senders: &BTreeSet<ConnectionId>,
    shared_scope: &BTreeMap<ConnectionId, Scope>,
    scope: Scope,
    receiver: ConnectionId,
) -> ConnectionId {
    senders
        .iter()
        .find(|sender| **sender != receiver && shared_scope.get(sender) == Some(&scope))
        .copied()
        .unwrap_or(receiver)
}

/// Whether `receiver` can see `sender`, through the shared view or its overlay.
fn can_see(
    receiver: ConnectionId,
    sender: ConnectionId,
    shared_scope: &BTreeMap<ConnectionId, Scope>,
    privately: &BTreeMap<ConnectionId, BTreeSet<ConnectionId>>,
    observations: &BTreeMap<ConnectionId, ScopeSet>,
) -> bool {
    if privately
        .get(&receiver)
        .is_some_and(|visible| visible.contains(&sender))
    {
        return true;
    }
    let Some(scope) = shared_scope.get(&sender) else {
        // The sender is rendered nowhere, so no receiver could know its session.
        return false;
    };
    observations
        .get(&receiver)
        .copied()
        .unwrap_or(ScopeSet::NONE)
        .sees(*scope)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::ids::SyntheticId;

    fn observations(pairs: &[(u64, ScopeSet)]) -> BTreeMap<ConnectionId, ScopeSet> {
        pairs
            .iter()
            .map(|(connection, see)| (ConnectionId(*connection), *see))
            .collect()
    }

    fn everything() -> ScopeSet {
        ScopeSet::new(&[Scope::ROOT]).expect("one scope")
    }

    #[test]
    fn a_flavor_offering_too_many_actions_fails_its_render_rather_than_the_connection() {
        // The whole turn goes into the queue in one piece, and an oversized batch
        // closes the connection. A refused render keeps the bug inside the flavor.
        let ids = SharedIds::new();
        let connections = [ConnectionId(1)];
        let mut builder = ShardBuilder::new(&ids, ShardId(1), &connections);
        builder.root("Lobby");
        builder.private(ConnectionId(1), |private| {
            for key in 0..=u64::try_from(MAX_ACTIONS).expect("small") {
                private.action(ActionKey(key), "Do it", On::SERVER);
            }
        });

        assert!(matches!(
            builder.finish(&observations(&[(1, everything())])),
            Err(BuildError::TooManyActions {
                connection: ConnectionId(1)
            })
        ));
    }

    #[test]
    fn offering_the_same_action_twice_keeps_the_last_word() {
        let ids = SharedIds::new();
        let connections = [ConnectionId(1)];
        let mut builder = ShardBuilder::new(&ids, ShardId(1), &connections);
        builder.root("Lobby");
        builder.private(ConnectionId(1), |private| {
            private.action(ActionKey(1), "First", On::SERVER);
            private.action(ActionKey(1), "Second", On::USER);
        });

        let rendered = builder
            .finish(&observations(&[(1, everything())]))
            .expect("a render with one action");
        let offered = &rendered.actions[&ConnectionId(1)];
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[&ActionKey(1)].text, "Second");
        assert_eq!(offered[&ActionKey(1)].on, On::USER);
    }

    #[test]
    fn a_user_is_always_at_or_below_its_channels_scope() {
        let ids = SharedIds::new();
        let connections = [ConnectionId(1)];
        let mut builder = ShardBuilder::new(&ids, ShardId(1), &connections);

        let root = builder.root("Lobby");
        let game = builder.channel(root, ChannelKey(1), "Game", Narrow::Into(7));
        let team = builder.channel(game, ChannelKey(2), "Team", Narrow::Into(2));
        builder.user(
            team,
            Occupant::Connection(ConnectionId(1)),
            "alice",
            Narrow::Into(42),
        );

        let rendered = builder
            .finish(&observations(&[(1, everything())]))
            .expect("a hierarchical render is always coherent");

        // The closure theorem, observed on the produced value: there is no
        // builder call that could have made this false.
        for user in rendered.view.users.values() {
            let channel = rendered
                .view
                .channels
                .get(&user.channel)
                .expect("a user's channel is always rendered");
            assert!(
                channel.scope.is_prefix_of(user.scope),
                "a user must never be broader than its channel"
            );
        }
        for channel in rendered.view.channels.values() {
            let parent = rendered
                .view
                .channels
                .get(&channel.parent)
                .expect("a channel's parent is always rendered");
            assert!(parent.scope.is_prefix_of(channel.scope));
        }
    }

    #[test]
    fn a_render_without_a_root_is_refused() {
        let ids = SharedIds::new();
        let builder = ShardBuilder::new(&ids, ShardId(1), &[]);
        assert_eq!(
            builder.finish(&BTreeMap::new()),
            Err(BuildError::MissingRoot)
        );
    }

    #[test]
    fn the_same_channel_key_twice_is_refused_rather_than_merged() {
        let ids = SharedIds::new();
        let mut builder = ShardBuilder::new(&ids, ShardId(1), &[]);
        let root = builder.root("Lobby");
        builder.channel(root, ChannelKey(1), "A", Narrow::Same);
        builder.channel(root, ChannelKey(1), "B", Narrow::Same);

        assert_eq!(
            builder.finish(&BTreeMap::new()),
            Err(BuildError::DuplicateChannelKey(ChannelKey(1)))
        );
    }

    #[test]
    fn narrowing_past_the_depth_bound_refuses_the_render() {
        let ids = SharedIds::new();
        let mut builder = ShardBuilder::new(&ids, ShardId(1), &[]);
        let mut current = builder.root("Lobby");
        for depth in 0..u64::try_from(crate::scope::MAX_DEPTH).unwrap_or(4) + 1 {
            current = builder.channel(
                current,
                ChannelKey(depth + 1),
                "deep",
                Narrow::Into(u32::try_from(depth).unwrap_or(0)),
            );
        }

        assert!(matches!(
            builder.finish(&BTreeMap::new()),
            Err(BuildError::ScopeTooDeep { .. })
        ));
    }

    #[test]
    fn linking_across_incomparable_scopes_is_refused() {
        let ids = SharedIds::new();
        let mut builder = ShardBuilder::new(&ids, ShardId(1), &[]);
        let root = builder.root("Lobby");
        let red = builder.channel(root, ChannelKey(1), "Red", Narrow::Into(2));
        let blue = builder.channel(root, ChannelKey(2), "Blue", Narrow::Into(3));
        builder.channel_link(red, blue);

        assert_eq!(
            builder.finish(&BTreeMap::new()),
            Err(BuildError::LinkAcrossScopes {
                a: red.id(),
                b: blue.id()
            })
        );
    }

    #[test]
    fn linking_within_comparable_scopes_is_symmetric() {
        let ids = SharedIds::new();
        let mut builder = ShardBuilder::new(&ids, ShardId(1), &[]);
        let root = builder.root("Lobby");
        let game = builder.channel(root, ChannelKey(1), "Game", Narrow::Into(7));
        let team = builder.channel(game, ChannelKey(2), "Team", Narrow::Into(2));
        builder.channel_link(game, team);

        let rendered = builder.finish(&BTreeMap::new()).expect("comparable link");
        assert!(
            rendered.view.channels[&game.id()]
                .links
                .contains(&team.id())
        );
        assert!(
            rendered.view.channels[&team.id()]
                .links
                .contains(&game.id())
        );
    }

    #[test]
    fn the_same_person_shared_and_private_is_refused_rather_than_merged() {
        let ids = SharedIds::new();
        let connections = [ConnectionId(1)];
        let mut builder = ShardBuilder::new(&ids, ShardId(1), &connections);
        let root = builder.root("Lobby");
        let admin = Occupant::Connection(ConnectionId(1));
        builder.user(root, admin, "admin", Narrow::Same);
        builder.private(ConnectionId(1), |private| {
            private.user_in(root, admin, "admin");
        });

        assert!(matches!(
            builder.finish(&observations(&[(1, everything())])),
            Err(BuildError::SharedAndPrivateUser { .. })
        ));
    }

    #[test]
    fn an_overlay_cannot_place_someone_in_a_channel_that_connection_cannot_see() {
        let ids = SharedIds::new();
        let connections = [ConnectionId(1)];
        let mut builder = ShardBuilder::new(&ids, ShardId(1), &connections);
        let root = builder.root("Lobby");
        let hidden = builder.channel(root, ChannelKey(1), "Red", Narrow::Into(2));
        builder.private(ConnectionId(1), |private| {
            private.user_in(hidden, Occupant::Synthetic(SyntheticId(1)), "ghost");
        });

        // The connection only observes the sibling team, so the placement
        // channel is invisible to it.
        let blind = ScopeSet::new(&[Scope::ROOT.child(3).expect("depth 1")]).expect("one scope");
        assert!(matches!(
            builder.finish(&observations(&[(1, blind)])),
            Err(BuildError::OverlayChannelInvisible { .. })
        ));
    }

    #[test]
    fn an_audio_edge_into_a_blind_receiver_is_refused() {
        let ids = SharedIds::new();
        let connections = [ConnectionId(1), ConnectionId(2)];
        let mut builder = ShardBuilder::new(&ids, ShardId(1), &connections);
        let root = builder.root("Lobby");
        let red = builder.channel(root, ChannelKey(1), "Red", Narrow::Into(2));
        let blue = builder.channel(root, ChannelKey(2), "Blue", Narrow::Into(3));
        builder.user(
            red,
            Occupant::Connection(ConnectionId(1)),
            "a",
            Narrow::Same,
        );
        builder.user(
            blue,
            Occupant::Connection(ConnectionId(2)),
            "b",
            Narrow::Same,
        );
        builder.audio_edge(ConnectionId(1), ConnectionId(2));

        let red_only = ScopeSet::new(&[Scope::ROOT.child(2).expect("depth 1")]).expect("one scope");
        let blue_only =
            ScopeSet::new(&[Scope::ROOT.child(3).expect("depth 1")]).expect("one scope");
        assert_eq!(
            builder.finish(&observations(&[(1, red_only), (2, blue_only)])),
            Err(BuildError::ReceiverCannotSeeSender {
                sender: ConnectionId(1),
                receiver: ConnectionId(2),
            })
        );
    }

    #[test]
    fn an_audio_edge_is_allowed_when_the_receiver_sees_the_sender_privately() {
        let ids = SharedIds::new();
        let connections = [ConnectionId(1), ConnectionId(2)];
        let mut builder = ShardBuilder::new(&ids, ShardId(1), &connections);
        let root = builder.root("Lobby");
        let red = builder.channel(root, ChannelKey(1), "Red", Narrow::Into(2));
        builder.user(
            red,
            Occupant::Connection(ConnectionId(2)),
            "b",
            Narrow::Same,
        );
        // The vanished admin exists only in connection 2's overlay, and that is
        // exactly what makes it audible to it.
        builder.private(ConnectionId(2), |private| {
            private.user_in(red, Occupant::Connection(ConnectionId(1)), "admin");
        });
        builder.audio_edge(ConnectionId(1), ConnectionId(2));

        let red_only = ScopeSet::new(&[Scope::ROOT.child(2).expect("depth 1")]).expect("one scope");
        assert!(
            builder
                .finish(&observations(&[(1, red_only), (2, red_only)]))
                .is_ok()
        );
    }
}
