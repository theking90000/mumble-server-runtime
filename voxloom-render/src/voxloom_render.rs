//! Pure view engine for Voxloom (roadmap Phase 5).
//!
//! This crate holds the normalized per-connection view (`ClientView`) and the
//! two pure operations the reconciler consumes: [`normalize`] (spec 12.3) and
//! [`validate`] (spec section 20 invariants). It is deliberately free of any
//! network, runtime, or wire-format concern:
//!
//! - it never imports the wire-format codec crate (enforced by `ci/gates.sh`,
//!   `render/no-protocol`): the renderer describes desired views in canonical,
//!   presentation-agnostic terms, and the mapping to Mumble messages lives in
//!   the reconciler and session layers;
//! - it has no IO and no async: every function here is a pure function over
//!   immutable data, which is what makes the whole engine property-testable
//!   (ADR-003: a full render is the correctness oracle).
//!
//! The `ChannelId`/`SessionId` newtypes here are *view* identifiers (per
//! connection, spec 9.1/9.2), not wire types. Assigning stable ids to semantic
//! keys is the reconciler's job (see `voxloom-reconcile::ViewIdMapping`,
//! ADR-007); by the time a `ClientView` reaches this crate the ids are already
//! resolved.

mod ids;
mod keys;
mod normalize;
mod validate;
mod view;

pub use ids::{ChannelId, SessionId};
pub use keys::{ActionKey, ChannelKey, SemanticKey, UserKey};
pub use normalize::normalize;
pub use validate::{Invariant, ValidationError, validate};
pub use view::{
    ActionTarget, BlobRef, ClientView, ContextActionView, ListenerRelation, PermissionBits,
    ServerPresentation, ViewChannel, ViewUser,
};
