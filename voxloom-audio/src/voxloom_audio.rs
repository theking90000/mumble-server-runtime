//! Pure audio routing for Voxloom (roadmap Phase 4).
//!
//! This crate holds the two halves of the audio data plane that can be written
//! as pure functions over immutable data, and nothing else. It owns no socket,
//! no cryptographic state and no synchronisation primitive: the session layer
//! keeps those, calls in with a snapshot, and applies the answers.
//!
//! The split is ADR-005 ("the router consults a local snapshot only", whose
//! consequence is "policies must be compiled when state changes"), which the
//! specification restates in 23.1/23.2:
//!
//! - **cold path**, [`compile`]: turn the current participants into an
//!   [`AudioRoutingSnapshot`]. Runs when membership or policy changes, which is
//!   the rate of presence events, not the rate of speech. It is allowed to be
//!   expensive.
//! - **hot path**, [`AudioRoutingSnapshot::receivers`] and [`may_receive`]: run
//!   once per datagram per recipient. A lookup is a hash probe plus a slice
//!   borrow; nothing is recomputed, nothing is allocated, nothing is shared
//!   mutably.
//!
//! Phase 4 deliberately ships a trivial policy (everyone in a domain hears
//! everyone else in it) published through the final mechanism. What has to be
//! right now is the *shape*: [`RoutingDomainId`] in every snapshot key, and a
//! packet path that reads a swapped-in snapshot instead of querying live state.
//! Replacing the body of [`compile`] with proximity or permissions (Phases 7
//! and 8) then touches nothing else.
//!
//! Opus payloads are never decoded here: routing rewrites envelope metadata and
//! forwards the encoded frames untouched (spec 15.2).

mod envelope;
mod policy;
mod snapshot;

pub use envelope::outgoing_audio;
pub use policy::{AudioContext, AudioDecision, AudioTarget, may_receive};
pub use snapshot::{
    AudioRoutingSnapshot, DirectedRoute, Participant, RouteMatrix, RoutingDomainId, SenderMetadata,
    SessionId, compile, compile_authorized,
};
