//! Pure reconciliation for Voxloom (roadmap Phase 5).
//!
//! Given a connection's committed view and the freshly rendered desired view,
//! this crate computes the logical difference ([`diff`], spec 12.4) and orders
//! it into a protocol-safe transition plan ([`plan`], spec 12.5/12.6), wrapped
//! in an [`OutputTransaction`] (spec 12.7). It also owns per-connection
//! key-to-id resolution with stability ([`ViewIdMapping`], ADR-007).
//!
//! Like `voxloom-render`, this crate is pure: no IO, no async, no wire format.
//! It depends only on the view types of `voxloom-render`. The plan it produces
//! is a sequence of abstract [`PlanOp`]s, not Mumble messages: turning a plan
//! into `UserState`/`ChannelState`/... frames is the session layer's job
//! (Phase 3). Keeping the plan abstract is what lets the ordering be tested
//! against a pure view applier instead of a live client.

mod diff;
mod idmap;
mod plan;

pub use diff::{ChannelPatch, ListenerUpdate, PermissionUpdate, UserPatch, ViewDelta, diff};
pub use idmap::{ChannelIdKind, IdError, ViewIdMapping};
pub use plan::{AudioRoute, OutputTransaction, PlanOp, plan};
