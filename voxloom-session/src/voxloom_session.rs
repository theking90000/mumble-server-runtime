//! Per-connection view lifecycle (roadmap Phase 6).
//!
//! One connection holds one view. This crate owns everything that follows from
//! that sentence: what the connection currently shows, which ids it uses for
//! which semantic keys, how it moves from one view to the next, and when that
//! move may be considered done. The server above it owns sockets, the registry
//! and the N connections; nothing here knows any of that exists.
//!
//! The crate is **pure**: no runtime, no sockets, no queue. That is what lets
//! the ordering and atomicity rules be tested against values instead of against
//! a live client, and it is why the admission decision is an *input* here rather
//! than a call out.
//!
//! Layering:
//! - [`emit`] translates an abstract [`voxloom_reconcile::PlanOp`] into the
//!   control messages that carry it. It is the only place that speaks both view
//!   and wire.
//! - [`view`] holds the committed view of one connection and the rule that
//!   governs advancing it: prepared, then committed only once the whole
//!   transition has been accepted downstream (spec 12.7, invariant 20).
#![forbid(unsafe_code)]

pub mod emit;
pub mod inbound;
pub mod view;

pub use emit::{EmitError, EmittedStep, emit_transaction, wire_permissions};
pub use inbound::{InboundCommand, InboundError, UnsupportedKind};
pub use view::{CommitToken, ConnectionView, PendingTransition, TransitionError};

// Re-exported so a consumer can name what comes out of a transition without
// depending on the view engine itself.
pub use voxloom_reconcile::AudioRoute;
pub use voxloom_render::{ChannelId, ClientView, SessionId};
