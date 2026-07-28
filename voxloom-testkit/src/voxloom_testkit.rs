//! Verifier toolkit for Mumble Server Runtime (roadmap Phase 3).
//!
//! The centrepiece is [`SimulatedMumbleClient`]: a real TLS Mumble client that
//! applies every server message to a strict [`ClientModel`] and **panics** on any
//! spec §20 invariant violation, exactly as the official client would reject a
//! malformed server. Per R2 it is written against the spec and the vendored
//! reference — not against the Mumble Server Runtime server — and is the machine judge for the
//! handshake now and for the reconciler property tests in later phases.
//!
//! This crate deliberately does not enable the workspace `unwrap/expect` lints:
//! its contract is to panic loudly with a diagnostic when an invariant breaks.
#![forbid(unsafe_code)]

pub mod client;
pub mod model;
pub mod tls;

pub use client::SimulatedMumbleClient;
pub use model::{ClientModel, ModelChannel, ModelUser};
