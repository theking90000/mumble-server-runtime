//! Voxloom MITM proxy (Phase 2): a living oracle between a Mumble client and a
//! real Murmur server. It terminates TLS on both sides, decodes and re-encodes
//! every control message (exercising the Phase 1 codec on live traffic), and
//! rewrites the OCB2 key exchange so it holds an independent cipher domain toward
//! each side. This crate is a research/oracle tool, not part of the runtime.
//!
//! Split into: [`session`] (the crypto-rewrite state machine, pure and testable),
//! [`relay`] (the framed async control-plane relay), [`udp`] (the pure UDP
//! re-encryption), [`udp_relay`] (the async UDP socket wiring and address ->
//! session correlation), and [`tls`] (TLS setup).

pub mod relay;
pub mod session;
pub mod tls;
pub mod udp;
pub mod udp_relay;

pub use relay::{Origin, pump_control, random_secrets, serve};
pub use session::{Action, CryptChannels, ProxySecrets, Session, SessionError};
pub use udp::{DropReason, UdpOutcome, reencrypt_from_client, reencrypt_from_server};
pub use udp_relay::{Registration, Registry, run_udp, serve_udp};
