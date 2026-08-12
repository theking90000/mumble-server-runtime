//! The front door of the shard runtime: sockets, shards, and the path a voice
//! packet takes between them.
//!
//! [`mumble_server_runtime_shard`] is pure in the way that matters: it renders, plans and
//! composes, and it never learns what a socket is. This crate is everything it
//! deliberately does not know.
//!
//! ```text
//!   TCP+TLS  -> connection task -> router.route() -> Attach(shard)
//!                     |                                   |
//!                     | control frames                    | commands, wake
//!                     v                                   v
//!               output queue  <-------------------  the shard's task
//!
//!   UDP      -> voice plane -> bindings -> the shard's routing table
//!                              (no shard task is involved, ever)
//! ```
//!
//! # The three things worth knowing before reading further
//!
//! **A shard task never waits for IO, and the voice plane never waits for a
//! shard.** The two planes meet at exactly two places: a routing table published
//! by [`mumble_server_runtime_shard::Shard`] and read without a shard's help, and a per
//! connection cursor the shard advances and the voice plane compares against.
//! Everything else is separate by construction rather than by discipline.
//!
//! **Identifiers are runtime-wide, not per shard.** A connection that moves
//! keeps its session, and two shards never hand the same wire number to two
//! different things. Both properties are load-bearing for migration, and the
//! reason is in [`mumble_server_runtime_shard::IdAllocator`].
//!
//! **A migration is not a disconnect followed by a connect.** The source hands
//! the destination the view the client still holds, and the destination plans
//! one transition onto it. Tearing down first would disconnect the official
//! client outright: see [`runtime::RuntimeHandle::move_connection`].
//!
//! REF: docs/design/guide-implementation.md 8, 9.6, 9.7, 10
#![forbid(unsafe_code)]

pub mod config;
pub mod connection;
pub mod handshake;
pub mod limits;
#[cfg(feature = "load-metrics")]
pub mod metrics;
pub mod peer;
pub mod router;
pub mod runtime;
pub mod serve;
pub mod tls;
pub mod voice;

pub use config::GatewayConfig;
#[cfg(feature = "load-metrics")]
pub use metrics::{VoiceMetrics, VoiceMetricsSnapshot};
pub use peer::{Peer, Peers, ShardPlane};
pub use router::{ConnectionIdentity, ConnectionRouter, RouteDecision};
pub use runtime::{Runtime, RuntimeHandle, ShardStatus};
pub use serve::{Gateway, serve};
pub use voice::VoicePlane;
