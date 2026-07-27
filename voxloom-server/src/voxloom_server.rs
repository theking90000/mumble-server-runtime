//! Minimal Mumble-compatible server (roadmap Phase 3).
//!
//! This crate brings up a server that a real Mumble 1.5+ client can connect to
//! without Murmur: it accepts TLS, drives the initial handshake, sets up UDP
//! encryption, answers pings and reflects loopback audio back to its sender.
//!
//! The wire is never spoken from memory (R1): all framing, message and envelope
//! encoding comes from `voxloom-protocol`, all OCB2 from `voxloom-crypto`, both
//! validated against the vendored reference and the corpus in earlier phases.
//! The handshake ordering here traces to Murmur's `Messages.cpp::msgAuthenticate`
//! and `Server.cpp::encrypted` (see `// REF:` notes in `handshake.rs`).
//!
//! Layering:
//! - [`handshake`] is a pure function producing the ordered server->client
//!   control messages. It has no IO, so the §20 ordering invariants are unit
//!   tested directly against it.
//! - [`state`] holds the transport-side state (session-id allocator, connected
//!   users, UDP bindings) and the publication coordinator. No business state:
//!   what a connection sees and hears is the compiled flavor's answer.
//! - [`flavor`] is the seam to that flavor. The server is compiled once and
//!   knows no business model; a composition binary picks the flavor.
//! - [`connection`] is the async per-connection task: TLS accept, read
//!   Version/Authenticate, emit the handshake, then service the connection.
//! - [`outbound`] is one connection's bounded output queue and the admission
//!   policy over it: voice is droppable, control is not. It is also the intended
//!   commit point for the per-connection view transactions of P6, and the module
//!   documentation records why, so that design is not re-derived from scratch.
//! - [`voice`] is the UDP voice plane: crypto association by proof, ping replies
//!   and loopback reflection.
#![forbid(unsafe_code)]

pub mod config;
pub mod connection;
pub mod flavor;
pub mod handshake;
pub mod limits;
pub mod outbound;
pub mod routing;
pub mod server;
pub mod state;
pub mod tls;
pub mod voice;

pub use config::ServerConfig;
pub use flavor::{FlavorRuntime, GenerationError};
pub use server::{Server, ServerHandle};
pub use state::{SessionId, SharedState};
