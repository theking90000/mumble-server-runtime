//! Identifiers, and the allocator that keeps them stable.
//!
//! Two rules are absolute on [`SessionId`] and [`ChannelId`]:
//!
//! - **Never reused.** A withdrawn identifier is dead forever, because the
//!   Mumble client attaches local preferences (nickname overrides, per-user
//!   volume, channel filter mode) to them.
//! - **[`ChannelId::ROOT`] is the root**, and it belongs to the runtime rather
//!   than to any shard.
//!
//! [`SessionId`] is deliberately not [`ConnectionId`]: not everyone visible is
//! connected. The relation is `SessionId` ⊇ `ConnectionId` - an NPC, a player
//! outside Mumble or a bot has a session but no socket, no key and no cursor.
//! That is what [`Occupant::Synthetic`] is for.
//!
//! REF: docs/design/guide-implementation.md 4

use std::collections::BTreeMap;

use thiserror::Error;

/// A shard: a unit of ownership, scheduling, rendering and id allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ShardId(pub u64);

/// A live connection: socket, crypto state, output queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnectionId(pub u64);

/// A visible user with no voice connection: NPC, out-of-Mumble player, bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SyntheticId(pub u64);

/// A visible user, as it goes on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(pub u32);

/// A channel, as it goes on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelId(pub u32);

impl ChannelId {
    /// The root channel. It exists client-side before any of our messages, and
    /// shards render subtrees beneath it.
    pub const ROOT: ChannelId = ChannelId(0);
}

/// A flavor-chosen stable identity for a channel.
///
/// # Why the builder asks for this
///
/// The guide's builder derives a channel's *scope* from its parent but never
/// says where its *identity* comes from. Something has to: the diff needs to
/// recognize the same channel across two renders, and an identity that drifts
/// burns a wire id every turn.
///
/// Deriving it from the name would be the obvious guess and is exactly the trap
/// the guide names (15, "never put changing content in an identity"): a channel
/// whose name carries a clock would be destroyed and recreated ten times a
/// second. So the flavor states the identity explicitly and keeps the name a
/// field. Two channels rendered in one turn with the same key is a build error,
/// not a merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelKey(pub u64);

/// Who occupies a rendered user slot.
///
/// This *is* the user's identity, which is why - unlike channels - users need no
/// separate key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Occupant {
    /// A player actually connected.
    Connection(ConnectionId),
    /// A visible user with no voice connection.
    Synthetic(SyntheticId),
}

impl Occupant {
    /// The connection behind this occupant, if any.
    #[must_use]
    pub fn connection(self) -> Option<ConnectionId> {
        match self {
            Occupant::Connection(connection) => Some(connection),
            Occupant::Synthetic(_) => None,
        }
    }
}

/// The identifier space is exhausted.
///
/// Reached only after 2^32 distinct channels or users in one shard's lifetime.
/// It refuses rather than wrapping, because wrapping *is* reuse and the client
/// would silently apply one user's local settings to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum Exhausted {
    #[error("this shard has allocated every channel id")]
    Channels,
    #[error("this shard has allocated every session id")]
    Sessions,
}

/// Maps stable identities onto wire identifiers, forever.
///
/// Entries are never removed. An element that disappears and comes back gets the
/// **same** id, which is not reuse: it is the same thing returning, and the
/// client's local preferences for it are still correct. Withdrawing the entry is
/// what would be unsafe, since the next allocation could hand its number to
/// something else.
#[derive(Debug)]
pub struct IdAllocator {
    channels: BTreeMap<ChannelKey, ChannelId>,
    sessions: BTreeMap<Occupant, SessionId>,
    /// The next number to hand out. `None` once the space is spent, which is a
    /// distinct state from "the last number", so the final id is actually used
    /// rather than refused.
    next_channel: Option<u32>,
    next_session: Option<u32>,
}

impl Default for IdAllocator {
    fn default() -> IdAllocator {
        IdAllocator::new()
    }
}

impl IdAllocator {
    /// A fresh allocator. Channel ids start at 1: zero is the runtime's root.
    #[must_use]
    pub fn new() -> IdAllocator {
        IdAllocator {
            channels: BTreeMap::new(),
            sessions: BTreeMap::new(),
            next_channel: Some(1),
            next_session: Some(1),
        }
    }

    /// The id for `key`, allocating one the first time it is seen.
    ///
    /// # Errors
    ///
    /// [`Exhausted::Channels`] once every id has been handed out.
    pub fn channel(&mut self, key: ChannelKey) -> Result<ChannelId, Exhausted> {
        if let Some(existing) = self.channels.get(&key) {
            return Ok(*existing);
        }
        let raw = self.next_channel.ok_or(Exhausted::Channels)?;
        self.next_channel = raw.checked_add(1);
        let id = ChannelId(raw);
        self.channels.insert(key, id);
        Ok(id)
    }

    /// The session for `occupant`, allocating one the first time it is seen.
    ///
    /// # Errors
    ///
    /// [`Exhausted::Sessions`] once every session has been handed out.
    pub fn session(&mut self, occupant: Occupant) -> Result<SessionId, Exhausted> {
        if let Some(existing) = self.sessions.get(&occupant) {
            return Ok(*existing);
        }
        let raw = self.next_session.ok_or(Exhausted::Sessions)?;
        self.next_session = raw.checked_add(1);
        let id = SessionId(raw);
        self.sessions.insert(occupant, id);
        Ok(id)
    }

    /// The session already allocated for `occupant`, without allocating.
    #[must_use]
    pub fn allocated_session(&self, occupant: Occupant) -> Option<SessionId> {
        self.sessions.get(&occupant).copied()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn the_same_identity_always_gets_the_same_id() {
        let mut ids = IdAllocator::new();
        let first = ids.channel(ChannelKey(7)).expect("allocatable");
        let other = ids.channel(ChannelKey(8)).expect("allocatable");
        let again = ids.channel(ChannelKey(7)).expect("allocatable");

        assert_eq!(first, again);
        assert_ne!(first, other);
    }

    #[test]
    fn channel_ids_never_collide_with_the_runtime_root() {
        let mut ids = IdAllocator::new();
        for key in 0..16 {
            let id = ids.channel(ChannelKey(key)).expect("allocatable");
            assert_ne!(id, ChannelId::ROOT, "the root belongs to the runtime");
        }
    }

    #[test]
    fn an_identity_that_vanishes_and_returns_keeps_its_id() {
        // Nothing withdraws an entry, so this is a statement about the whole
        // type: there is no removal path that could free a number for reuse.
        let mut ids = IdAllocator::new();
        let alice = Occupant::Connection(ConnectionId(1));
        let before = ids.session(alice).expect("allocatable");

        for other in 2..10 {
            let _ = ids.session(Occupant::Connection(ConnectionId(other)));
        }

        assert_eq!(ids.session(alice).expect("allocatable"), before);
    }

    #[test]
    fn connections_and_synthetics_share_the_session_space_without_colliding() {
        let mut ids = IdAllocator::new();
        let connected = ids
            .session(Occupant::Connection(ConnectionId(3)))
            .expect("allocatable");
        let synthetic = ids
            .session(Occupant::Synthetic(SyntheticId(3)))
            .expect("allocatable");

        assert_ne!(
            connected, synthetic,
            "the same raw number in two occupant kinds is two different users"
        );
    }

    #[test]
    fn exhaustion_refuses_rather_than_wrapping() {
        let mut ids = IdAllocator::new();
        ids.next_channel = Some(u32::MAX);

        // The very last number must actually be handed out: refusing it would
        // be an off-by-one that silently costs a channel.
        let last = ids.channel(ChannelKey(1)).expect("one left");
        assert_eq!(last, ChannelId(u32::MAX));
        assert_eq!(ids.channel(ChannelKey(2)), Err(Exhausted::Channels));

        // And an identity already allocated still resolves after exhaustion.
        assert_eq!(ids.channel(ChannelKey(1)), Ok(last));
    }

    #[test]
    fn looking_up_without_allocating_does_not_allocate() {
        let mut ids = IdAllocator::new();
        let occupant = Occupant::Connection(ConnectionId(5));
        assert_eq!(ids.allocated_session(occupant), None);

        let session = ids.session(occupant).expect("allocatable");
        assert_eq!(ids.allocated_session(occupant), Some(session));
    }
}
