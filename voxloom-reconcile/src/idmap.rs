//! Per-connection stable key-to-id resolution (ADR-007, spec 9.2/9.3).
//!
//! A channel id is specific to a connection but must stay stable for a given
//! view key, or the official client's local caches (per-Channel-ID preferences,
//! shortcuts, desired reconnect channel) get confused (spec 9.3, 21.3). This
//! mapping gives every semantic key a durable id and never reuses a released id
//! (spec 20 invariant 12), by allocating monotonically from two ranges:
//!
//! - semantically stable channels get ids in `[1, EPHEMERAL_BASE)`;
//! - ephemeral channels get ids in `[EPHEMERAL_BASE, u32::MAX)`.
//!
//! The root id `0` is reserved and never allocated. Allocation is fail-closed:
//! exhausting a range returns [`IdError`], never wraps or reuses (L4/R6).

use std::collections::HashMap;

use voxloom_render::{ChannelId, ChannelKey};

/// First id of the ephemeral range. Stable ids stay strictly below it; the root
/// (`0`) sits below the stable range's first allocation (`1`).
const EPHEMERAL_BASE: u32 = 1 << 30;

/// Which id range a key draws from. The caller decides from the channel's
/// semantics (spec 9.3), not from the key's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelIdKind {
    /// A semantically durable channel: draws a low, stable id.
    Stable,
    /// A short-lived channel: draws from the reserved ephemeral range.
    Ephemeral,
}

/// Id allocation failure. Returned rather than wrapping so an exhausted id space
/// fails closed instead of silently reusing an id (spec 20 invariant 12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("stable channel id range is exhausted")]
    StableRangeExhausted,
    #[error("ephemeral channel id range is exhausted")]
    EphemeralRangeExhausted,
}

/// Per-connection channel id mapping (spec 9.2 `struct ViewIdMapping`).
#[derive(Debug, Clone, Default)]
pub struct ViewIdMapping {
    channel_to_view: HashMap<ChannelKey, ChannelId>,
    view_to_channel: HashMap<ChannelId, ChannelKey>,
    next_stable: u32,
    next_ephemeral: u32,
}

impl ViewIdMapping {
    /// A fresh mapping with both allocation cursors at the start of their range.
    #[must_use]
    pub fn new() -> ViewIdMapping {
        ViewIdMapping {
            channel_to_view: HashMap::new(),
            view_to_channel: HashMap::new(),
            next_stable: 1,
            next_ephemeral: EPHEMERAL_BASE,
        }
    }

    /// Resolve `key` to its id, allocating a fresh one on first use. Resolving
    /// the same key again always returns the same id (stability, ADR-007); the
    /// `kind` argument is only consulted for the first, allocating call.
    pub fn resolve(&mut self, key: ChannelKey, kind: ChannelIdKind) -> Result<ChannelId, IdError> {
        if let Some(id) = self.channel_to_view.get(&key) {
            return Ok(*id);
        }
        let id = match kind {
            ChannelIdKind::Stable => {
                if self.next_stable >= EPHEMERAL_BASE {
                    return Err(IdError::StableRangeExhausted);
                }
                let id = ChannelId(self.next_stable);
                // Cannot overflow: guarded above against EPHEMERAL_BASE < u32::MAX.
                self.next_stable += 1;
                id
            }
            ChannelIdKind::Ephemeral => {
                let next = self
                    .next_ephemeral
                    .checked_add(1)
                    .ok_or(IdError::EphemeralRangeExhausted)?;
                let id = ChannelId(self.next_ephemeral);
                self.next_ephemeral = next;
                id
            }
        };
        self.channel_to_view.insert(key.clone(), id);
        self.view_to_channel.insert(id, key);
        Ok(id)
    }

    /// Release `key`'s id when its channel disappears. The id is retired for the
    /// life of this mapping: monotonic cursors never hand it out again, so a
    /// later channel cannot inherit a stale client-side cache (invariant 12).
    /// Returns the released id, if the key was mapped.
    pub fn release(&mut self, key: &ChannelKey) -> Option<ChannelId> {
        let id = self.channel_to_view.remove(key)?;
        self.view_to_channel.remove(&id);
        Some(id)
    }

    /// The id currently bound to `key`, if any.
    #[must_use]
    pub fn get(&self, key: &ChannelKey) -> Option<ChannelId> {
        self.channel_to_view.get(key).copied()
    }

    /// The key currently bound to `id`, if any. This is how an inbound
    /// client-provided id is resolved back to a canonical key; a raw client id
    /// is never treated as canonical (ADR-007).
    #[must_use]
    pub fn key_of(&self, id: ChannelId) -> Option<&ChannelKey> {
        self.view_to_channel.get(&id)
    }
}
