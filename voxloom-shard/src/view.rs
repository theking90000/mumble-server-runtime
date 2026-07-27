//! The rendered shard view, and the private overlays layered on top of it.
//!
//! A [`ShardView`] holds each fact **once**, whoever ends up seeing it. That is
//! the whole reason the model is linear: `W` counts facts, while materializing
//! one view per connection would count copies.
//!
//! An [`Overlay`] holds the elements visible to exactly one connection. It is
//! never journalled and never merged into the shared view: an element is shared
//! **xor** private (guide 3.4), and [`crate::build`] enforces that.

use std::collections::{BTreeMap, BTreeSet};

use crate::ids::{ActionKey, ChannelId, ChannelKey, Occupant, SessionId};
use crate::scope::{Scope, ScopeSet};

/// A channel in the shared view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    /// Stable identity across renders.
    pub key: ChannelKey,
    pub id: ChannelId,
    /// The root is its own parent.
    pub parent: ChannelId,
    /// Position in the visibility tree. Extends the parent's.
    pub scope: Scope,
    pub name: String,
    pub position: i32,
    /// UI hint only: whether this connection may enter. Never a substitute for
    /// validating an actual join.
    pub can_enter: bool,
    pub links: BTreeSet<ChannelId>,
}

/// The boolean user state Mumble carries on `UserState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UserFlags {
    pub mute: bool,
    pub deaf: bool,
    pub suppress: bool,
    pub self_mute: bool,
    pub self_deaf: bool,
    pub priority_speaker: bool,
    pub recording: bool,
}

/// A user in the shared view, or in one connection's overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    /// Stable identity across renders.
    pub occupant: Occupant,
    pub session: SessionId,
    /// The channel the user is shown in. Must be visible wherever the user is.
    pub channel: ChannelId,
    /// Position in the visibility tree. Extends the channel's.
    pub scope: Scope,
    pub name: String,
    pub flags: UserFlags,
}

/// Everything one shard renders, each fact held once.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShardView {
    pub channels: BTreeMap<ChannelId, Channel>,
    pub users: BTreeMap<SessionId, User>,
}

impl ShardView {
    /// The view a connection holds before it has been told anything.
    #[must_use]
    pub fn empty() -> ShardView {
        ShardView::default()
    }

    /// The part of this view visible to `see`.
    ///
    /// Used only on the slow path (guide 9.4), where a connection has to learn a
    /// whole new subtree. The fast path never materializes a per-connection view
    /// at all - that is the point of the design.
    #[must_use]
    pub fn restrict(&self, see: ScopeSet) -> ShardView {
        ShardView {
            channels: self
                .channels
                .iter()
                .filter(|(_, channel)| see.sees(channel.scope))
                .map(|(id, channel)| (*id, channel.clone()))
                .collect(),
            users: self
                .users
                .iter()
                .filter(|(_, user)| see.sees(user.scope))
                .map(|(session, user)| (*session, user.clone()))
                .collect(),
        }
    }

    /// This view with `overlay` layered on top.
    ///
    /// There is nothing to reconcile: shared and private are disjoint by
    /// construction (guide 3.4), so this is a union, and an element appearing in
    /// both would be a build error caught long before here.
    #[must_use]
    pub fn compose(&self, overlay: &Overlay) -> ShardView {
        let mut composed = self.clone();
        composed
            .channels
            .extend(overlay.channels.iter().map(|(id, c)| (*id, c.clone())));
        composed
            .users
            .extend(overlay.users.iter().map(|(s, u)| (*s, u.clone())));
        composed
    }
}

/// Elements visible to exactly one connection.
///
/// This is the mechanism for individual exceptions - an admin in vanish, a
/// private channel, a per-observer placement - as opposed to scopes, which
/// describe groups. A scope with a single observer is an overlay in disguise.
///
/// Overlays are recomputed from scratch each turn and diffed against what the
/// connection was last sent, which is why they never need journalling.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Overlay {
    pub channels: BTreeMap<ChannelId, Channel>,
    pub users: BTreeMap<SessionId, User>,
}

impl Overlay {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty() && self.users.is_empty()
    }
}

/// Where a context action is offered in the client's interface.
///
/// A bit set rather than an enum: one action may be offered in several places at
/// once, which is exactly what the client's three menus do with it.
///
/// REF: references/vendored/Mumble.proto : `ContextActionModify.Context`,
///   `Server = 0x01`, `Channel = 0x02`, `User = 0x04`.
/// REF: references/mumble/src/mumble/Messages.cpp : `msgContextActionModify`
///   appends the same action to `qlServerActions`, `qlUserActions` and
///   `qlChannelActions`, one per bit set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct On(u32);

impl On {
    /// Offered on the server itself, with nothing selected.
    pub const SERVER: On = On(0x01);
    /// Offered when a channel is selected, and told which one.
    pub const CHANNEL: On = On(0x02);
    /// Offered when a user is selected, and told which one.
    pub const USER: On = On(0x04);

    /// Both places at once, and any other combination.
    #[must_use]
    pub fn and(self, other: On) -> On {
        On(self.0 | other.0)
    }

    /// Whether this action was offered in that place. The invocation is checked
    /// against it, so a client cannot invoke a user action on a channel.
    #[must_use]
    pub fn covers(self, place: On) -> bool {
        self.0 & place.0 == place.0
    }

    #[must_use]
    pub fn bits(self) -> u32 {
        self.0
    }
}

/// One action a flavor offers to one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub key: ActionKey,
    /// What the client writes in the menu. A field, not the identity.
    pub text: String,
    pub on: On,
}

/// The actions offered to one connection.
///
/// Private by construction, like an [`Overlay`], and recomputed from scratch
/// each turn: what a flavor offers may depend on who is asking, which is the
/// whole point of a button.
pub type Actions = BTreeMap<ActionKey, Action>;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::ids::ConnectionId;

    fn scope(segments: &[u32]) -> Scope {
        let mut scope = Scope::ROOT;
        for segment in segments {
            scope = scope.child(*segment).expect("within MAX_DEPTH");
        }
        scope
    }

    fn channel(id: u32, scope: Scope) -> Channel {
        Channel {
            key: ChannelKey(u64::from(id)),
            id: ChannelId(id),
            parent: ChannelId::ROOT,
            scope,
            name: format!("channel-{id}"),
            position: 0,
            can_enter: true,
            links: BTreeSet::new(),
        }
    }

    fn user(session: u32, channel: u32, scope: Scope) -> User {
        User {
            occupant: Occupant::Connection(ConnectionId(u64::from(session))),
            session: SessionId(session),
            channel: ChannelId(channel),
            scope,
            name: format!("user-{session}"),
            flags: UserFlags::default(),
        }
    }

    fn sample() -> ShardView {
        let mut view = ShardView::empty();
        view.channels.insert(ChannelId(1), channel(1, scope(&[7])));
        view.channels
            .insert(ChannelId(2), channel(2, scope(&[7, 2])));
        view.channels
            .insert(ChannelId(3), channel(3, scope(&[7, 3])));
        view.users
            .insert(SessionId(10), user(10, 2, scope(&[7, 2])));
        view.users
            .insert(SessionId(11), user(11, 3, scope(&[7, 3])));
        view
    }

    #[test]
    fn restricting_keeps_exactly_the_comparable_elements() {
        let view = sample();
        let team_two = ScopeSet::new(&[scope(&[7, 2])]).expect("one scope");
        let restricted = view.restrict(team_two);

        // The team's own channel plus every ancestor channel, and only the
        // teammate. The sibling team is gone in both maps.
        assert!(restricted.channels.contains_key(&ChannelId(1)));
        assert!(restricted.channels.contains_key(&ChannelId(2)));
        assert!(!restricted.channels.contains_key(&ChannelId(3)));
        assert!(restricted.users.contains_key(&SessionId(10)));
        assert!(!restricted.users.contains_key(&SessionId(11)));
    }

    #[test]
    fn a_restricted_view_never_strands_a_user_without_its_channel() {
        // This is the closure theorem observed rather than proved: whatever the
        // observation, no surviving user references a channel that was filtered
        // out. There is no runtime check anywhere that makes this true.
        let view = sample();
        let observations = [
            ScopeSet::new(&[scope(&[])]).expect("one scope"),
            ScopeSet::new(&[scope(&[7])]).expect("one scope"),
            ScopeSet::new(&[scope(&[7, 2])]).expect("one scope"),
            ScopeSet::new(&[scope(&[7, 3])]).expect("one scope"),
            ScopeSet::new(&[scope(&[8])]).expect("one scope"),
        ];

        for see in observations {
            let restricted = view.restrict(see);
            for user in restricted.users.values() {
                assert!(
                    restricted.channels.contains_key(&user.channel),
                    "user {:?} lost its channel under {see:?}",
                    user.session
                );
            }
        }
    }

    #[test]
    fn composing_layers_the_overlay_over_the_shared_view() {
        let view = sample();
        let mut overlay = Overlay::default();
        overlay
            .users
            .insert(SessionId(99), user(99, 2, scope(&[7, 2])));

        let composed = view.compose(&overlay);
        assert!(composed.users.contains_key(&SessionId(99)));
        assert!(composed.users.contains_key(&SessionId(10)));
        assert!(
            !view.users.contains_key(&SessionId(99)),
            "composing must not mutate the shared view"
        );
    }
}
