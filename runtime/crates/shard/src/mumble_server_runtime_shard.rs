//! A shard runtime: render once per shard, share the **changes**, not the views.
//!
//! # The model in one page
//!
//! The pipeline this crate replaces rendered *the whole world as seen by one
//! connection*, once per connection. Everything else followed mechanically: N
//! connections each holding an O(N) view means materializing every view costs
//! Θ(N²), and no scheduler or cache fixes that, because it is in the type.
//!
//! Here, a shard renders **once**. Each fact is held once, whoever ends up
//! seeing it. What connections receive is the resulting delta, filtered:
//!
//! ```text
//!   business state changes
//!        |  handle.wake()
//!        v
//!   render -> plan -> journal -> per-connection: filter, splice, collapse
//! ```
//!
//! A three-operation delta filtered N times costs O(N·|D|), not O(N·W). That
//! property holds even when every view is different, which is what makes it
//! robust rather than merely fast.
//!
//! # Three independent mechanisms
//!
//! Wanting to unify them is the mistake that breeds special cases.
//!
//! | mechanism | shape | what it solves | cost |
//! |---|---|---|---|
//! | [shared view + scopes](scope) | a scope tree, one scope per element, a `ScopeSet` per observer | parties, teams, spectators, staff - the 95% | O(W) once + O(\|D\|) per connection |
//! | [private overlay](view::Overlay) | a few elements visible to **one** connection | vanish, private channel, per-observer placement | O(\|overlay\|) |
//! | [audio relation](routing) | a **directed** relation per receiver | all of the audio | O(N + edges) |
//!
//! The rule that says which to use: **a scope describes a group, an overlay
//! describes an individual exception.** A scope with one observer is an overlay
//! in disguise. A role - player, host, spectator, staff - is a group by nature
//! even when it has one member.
//!
//! # The only coupling, and it is not ours
//!
//! > **A receiver must see the sender.**
//!
//! The Mumble client discards audio whose sender session it does not know, so
//! this is a protocol constraint rather than a design choice, and it is checked
//! on the render's output. A corollary worth stating: there is no separate
//! "right to speak". Speaking somewhere implies being visible there.
//!
//! REF: docs/design/guide-implementation.md
//! REF: references/mumble/src/mumble/ServerHandler.cpp : `handleVoicePacket`
#![forbid(unsafe_code)]

pub mod build;
pub mod compose;
pub mod emit;
pub mod ids;
pub mod journal;
pub mod plan;
pub mod queue;
pub mod reply;
pub mod routing;
pub mod scope;
pub mod shard;
pub mod view;

pub use build::{
    BuildError, ChannelRef, MAX_ACTIONS, Narrow, PrivateBuilder, Rendered, ShardBuilder, UserRef,
};
pub use compose::{collapse, filter, splice};
pub use emit::{
    TextTarget, action_key, action_name, actions, denied_permission, emit, perm, permission_query,
    permissions_of, relayed, spoken, user_stats,
};
pub use ids::{
    ActionKey, ChannelId, ChannelKey, ConnectionId, Exhausted, IdAllocator, Occupant, SessionId,
    ShardId, SharedIds, SyntheticId,
};
pub use journal::{Journal, TooFarBehind};
pub use plan::{
    ChannelPatch, ElementId, OverlayOps, PlanOp, PlannedOp, UserPatch, plan, plan_elements,
};
pub use queue::{MAX_DEPTH_FOR_VOICE, OutboundQueue, Refused, VoiceAdmission};
pub use reply::{Audience, Effect, Effects, Reply, Spoken, Word};
pub use routing::{AudioRelation, AudioRouting, DomainId, Silence, compile};
pub use scope::{MAX_DEPTH, MAX_OBSERVED, Scope, ScopeSet, TooManyScopes};
pub use shard::{
    ActionTarget, AttachedConnection, Handover, MIN_INTERVAL, ReconcileReport, Shard, ShardCommand,
    ShardHandle, ShardLogic, VoiceEvent, run, run_with_reports, spawn_parts,
};
pub use view::{Action, Actions, Channel, On, Overlay, ShardView, User, UserFlags};
