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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

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

impl ChannelKey {
    /// The key [`crate::build::ShardBuilder::root`] gives the shard's root.
    ///
    /// Named because a flavor addressing its whole tree has to spell it, and a
    /// bare zero in that position reads like a mistake.
    pub const ROOT: ChannelKey = ChannelKey(0);
}

/// A flavor's name for a context action, stable across renders.
///
/// Same idea as [`ChannelKey`], for the same reason: the wire identifier is a
/// free-form string the client stores and echoes back, and a flavor that used
/// the label as the identifier would break its own buttons the day it renames
/// one. The label stays a field; this is the identity.
///
/// REF: references/vendored/Mumble.proto : `ContextActionModify.action` and
///   `ContextAction.action` are the same string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActionKey(pub u64);

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
/// Reached only after 2^32 distinct channels or users in one runtime's lifetime.
/// It refuses rather than wrapping, because wrapping *is* reuse and the client
/// would silently apply one user's local settings to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum Exhausted {
    #[error("this runtime has allocated every channel id")]
    Channels,
    #[error("this runtime has allocated every session id")]
    Sessions,
}

/// Maps stable identities onto wire identifiers until a channel is withdrawn.
///
/// Session entries live forever because an official client keeps its own user
/// across migrations. Channel entries do not: once any client has accepted a
/// `ChannelRemove`, that wire identifier is dead even if the same semantic
/// channel later returns. Retiring a mapping never recycles its number because
/// the allocation cursor only moves forward.
///
/// # Why one allocator serves the whole runtime
///
/// Allocating per shard looks natural - a shard owns its subtree - and it is
/// wrong as soon as a connection can move between shards. The client keys its
/// model on the wire id, so shard A withdrawing channel 5 and shard B later
/// creating its own channel 5 is, from the client's seat, one identifier coming
/// back as a different thing. Sessions are worse: the official client refuses to
/// remove **itself** from its model, so a migrating connection that changed
/// session would be a second user forever.
///
/// Keying channels on `(shard, key)` and sessions on [`Occupant`] fixes both at
/// once. A migration keeps its session for free, because the occupant did not
/// change.
///
/// REF: references/mumble/src/mumble/Messages.cpp : `MainWindow::msgUserRemove`
///   ends with `if (pDst != pSelf) pmModel->removeUser(pDst);`.
#[derive(Debug)]
pub struct IdAllocator {
    channels: BTreeMap<(ShardId, ChannelKey), ChannelId>,
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

    /// The id for `key` within `shard`, allocating one the first time it is seen.
    ///
    /// # Errors
    ///
    /// [`Exhausted::Channels`] once every id has been handed out.
    pub fn channel(&mut self, shard: ShardId, key: ChannelKey) -> Result<ChannelId, Exhausted> {
        if let Some(existing) = self.channels.get(&(shard, key)) {
            return Ok(*existing);
        }
        let raw = self.next_channel.ok_or(Exhausted::Channels)?;
        self.next_channel = raw.checked_add(1);
        let id = ChannelId(raw);
        self.channels.insert((shard, key), id);
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

    /// The id already allocated for `key` within `shard`, without allocating.
    ///
    /// What a lookup outside a render needs: asking [`IdAllocator::channel`]
    /// there would hand out an id for a key nothing rendered, and an id handed
    /// out is an id spent for the life of the runtime.
    #[must_use]
    pub fn allocated_channel(&self, shard: ShardId, key: ChannelKey) -> Option<ChannelId> {
        self.channels.get(&(shard, key)).copied()
    }

    /// Retire every channel identity from `shard` that the accepted render no
    /// longer contains.
    ///
    /// Removing the mapping does not recycle its numeric id: `next_channel`
    /// only moves forward. If the same semantic key returns later, it therefore
    /// receives a fresh id, as required after the client has observed a
    /// `ChannelRemove`.
    pub fn retain_channels(&mut self, shard: ShardId, retained: &BTreeSet<ChannelKey>) {
        self.channels
            .retain(|(owner, key), _| *owner != shard || retained.contains(key));
    }

    /// Retire channel ids one client has accepted as removed.
    pub fn retire_channel_ids(&mut self, shard: ShardId, retired: &BTreeSet<ChannelId>) {
        self.channels
            .retain(|(owner, _), id| *owner != shard || !retired.contains(id));
    }
}

/// One allocator, shared by every shard of a runtime.
///
/// The lock is taken per allocation rather than for a whole render, so two
/// shards rendering on two cores contend for a few nanoseconds at a time instead
/// of serializing. It is never held across an `.await`: every method here
/// returns before the caller can suspend.
#[derive(Debug, Clone, Default)]
pub struct SharedIds {
    inner: Arc<Mutex<IdAllocator>>,
}

impl SharedIds {
    #[must_use]
    pub fn new() -> SharedIds {
        SharedIds::default()
    }

    /// The id for `key` within `shard`.
    ///
    /// # Errors
    ///
    /// [`Exhausted::Channels`] once every id has been handed out.
    pub fn channel(&self, shard: ShardId, key: ChannelKey) -> Result<ChannelId, Exhausted> {
        self.guard().channel(shard, key)
    }

    /// The session for `occupant`, stable for as long as the runtime lives.
    ///
    /// # Errors
    ///
    /// [`Exhausted::Sessions`] once every session has been handed out.
    pub fn session(&self, occupant: Occupant) -> Result<SessionId, Exhausted> {
        self.guard().session(occupant)
    }

    /// The session already allocated for `occupant`, without allocating.
    #[must_use]
    pub fn allocated_session(&self, occupant: Occupant) -> Option<SessionId> {
        self.guard().allocated_session(occupant)
    }

    /// The id already allocated for `key` within `shard`, without allocating.
    #[must_use]
    pub fn allocated_channel(&self, shard: ShardId, key: ChannelKey) -> Option<ChannelId> {
        self.guard().allocated_channel(shard, key)
    }

    /// Retire the channel identities absent from one shard's accepted render.
    pub fn retain_channels(&self, shard: ShardId, retained: &BTreeSet<ChannelKey>) {
        self.guard().retain_channels(shard, retained);
    }

    /// Retire channel ids one client has accepted as removed.
    pub fn retire_channel_ids(&self, shard: ShardId, retired: &BTreeSet<ChannelId>) {
        self.guard().retire_channel_ids(shard, retired);
    }

    /// A poisoned allocator means a panic unwound while an id was being handed
    /// out. The maps are still structurally sound - nothing here can leave one
    /// half-updated - and refusing every later allocation would take the whole
    /// runtime down over one thread, so the guard is recovered rather than
    /// propagated.
    fn guard(&self) -> MutexGuard<'_, IdAllocator> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn the_same_identity_always_gets_the_same_id() {
        let mut ids = IdAllocator::new();
        let first = ids.channel(ShardId(1), ChannelKey(7)).expect("allocatable");
        let other = ids.channel(ShardId(1), ChannelKey(8)).expect("allocatable");
        let again = ids.channel(ShardId(1), ChannelKey(7)).expect("allocatable");

        assert_eq!(first, again);
        assert_ne!(first, other);
    }

    #[test]
    fn a_channel_that_returns_after_removal_gets_a_fresh_id() {
        let mut ids = IdAllocator::new();
        let shard = ShardId(1);
        let key = ChannelKey(7);
        let before = ids.channel(shard, key).expect("allocatable");

        ids.retain_channels(shard, &BTreeSet::new());
        let after = ids.channel(shard, key).expect("allocatable");

        assert_ne!(before, after, "a retired wire id is dead forever");
    }

    #[test]
    fn retaining_one_shard_does_not_retire_another_shards_channels() {
        let mut ids = IdAllocator::new();
        let key = ChannelKey(7);
        let here = ids.channel(ShardId(1), key).expect("allocatable");
        let there = ids.channel(ShardId(2), key).expect("allocatable");

        ids.retain_channels(ShardId(1), &BTreeSet::new());

        assert_ne!(ids.channel(ShardId(1), key).expect("allocatable"), here);
        assert_eq!(ids.channel(ShardId(2), key), Ok(there));
    }

    #[test]
    fn retiring_a_wire_id_rekeys_only_its_channel() {
        let mut ids = IdAllocator::new();
        let shard = ShardId(1);
        let retired_key = ChannelKey(7);
        let kept_key = ChannelKey(8);
        let retired = ids.channel(shard, retired_key).expect("allocatable");
        let kept = ids.channel(shard, kept_key).expect("allocatable");

        ids.retire_channel_ids(shard, &BTreeSet::from([retired]));

        assert_ne!(ids.channel(shard, retired_key), Ok(retired));
        assert_eq!(ids.channel(shard, kept_key), Ok(kept));
    }

    #[test]
    fn channel_ids_never_collide_with_the_runtime_root() {
        let mut ids = IdAllocator::new();
        for key in 0..16 {
            let id = ids
                .channel(ShardId(1), ChannelKey(key))
                .expect("allocatable");
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
        let last = ids.channel(ShardId(1), ChannelKey(1)).expect("one left");
        assert_eq!(last, ChannelId(u32::MAX));
        assert_eq!(
            ids.channel(ShardId(1), ChannelKey(2)),
            Err(Exhausted::Channels)
        );

        // And an identity already allocated still resolves after exhaustion.
        assert_eq!(ids.channel(ShardId(1), ChannelKey(1)), Ok(last));
    }

    #[test]
    fn the_same_key_in_two_shards_is_two_channels() {
        // Without this, a connection migrating from one shard to the other
        // would be told to remove channel 5 and then to create channel 5 as a
        // different thing, which is identifier reuse seen from the client.
        let mut ids = IdAllocator::new();
        let here = ids.channel(ShardId(1), ChannelKey(7)).expect("allocatable");
        let there = ids.channel(ShardId(2), ChannelKey(7)).expect("allocatable");

        assert_ne!(here, there);
    }

    #[test]
    fn a_connection_keeps_its_session_across_shards() {
        // The session is keyed on the occupant, which does not change when a
        // connection moves. The official client never removes itself from its
        // own model, so a session that changed mid-connection would leave a
        // ghost behind forever.
        let ids = SharedIds::new();
        let who = Occupant::Connection(ConnectionId(4));

        let in_lobby = ids.session(who).expect("allocatable");
        let in_match = ids.session(who).expect("allocatable");

        assert_eq!(in_lobby, in_match);
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
