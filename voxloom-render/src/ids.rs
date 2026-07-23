//! View identifiers.
//!
//! These are per-connection presentation ids (spec 9.1 session ids, 9.2 channel
//! ids), not Mumble wire types. They are plain `u32` newtypes so a view can be
//! keyed and diffed deterministically; the reconciler allocates them from
//! semantic keys with stability guarantees (ADR-007).

/// Per-connection channel identifier. The root channel is always [`ChannelId::ROOT`].
///
/// REF: docs/voxloom-specification-technique-v0.1.md 9.2 (per-connection channel
///      ids, no immediate reuse) and 8.3 (`root_channel: ChannelId`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelId(pub u32);

impl ChannelId {
    /// The root channel id. Mumble's tree is always rooted at channel `0`.
    ///
    /// REF: docs/voxloom-specification-technique-v0.1.md 20 invariant 1
    ///      ("Le canal racine `0` existe").
    pub const ROOT: ChannelId = ChannelId(0);
}

/// Per-connection user session identifier (spec 9.1: monotonic `u32`, stable for
/// the connection, not reused quickly, known to the client before its audio).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(pub u32);
