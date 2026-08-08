//! What the gateway advertises during the handshake.
//!
//! Fixed for the life of the process. Everything a *flavor* would want to change
//! at runtime - names, trees, who hears whom - lives in the shard's render, not
//! here: this struct holds only the handful of values the Mumble handshake
//! demands before any view exists.

use std::net::SocketAddr;

/// Server-wide configuration advertised during the handshake.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// Where the TLS control plane listens. The voice plane binds the same port
    /// on UDP, as Murmur does.
    pub bind: SocketAddr,
    /// Welcome text sent in `ServerSync`. Empty means "omit the field".
    pub welcome_text: String,
    /// Advertised maximum bandwidth (bits/s), echoed to the client in
    /// `ServerSync` and `ServerConfig`.
    pub max_bandwidth: u32,
    /// Advertised maximum number of users (`ServerConfig.max_users`). Also the
    /// admission ceiling: past it, connections are refused rather than accepted
    /// into a runtime that advertised a smaller number.
    pub max_users: u32,
    /// Whether the client's built-in recording feature is announced as allowed
    /// (`ServerConfig.recording_allowed`). Advisory only (spec 21.8).
    pub recording_allowed: bool,
    /// Whether HTML is allowed in text (`ServerConfig.allow_html`).
    pub allow_html: bool,
    /// Maximum text-message length (`ServerConfig.message_length`).
    pub message_length: u32,
    /// Advertised server version (major, minor, patch). Defaults to 1.5.0 so a
    /// real client negotiates the protobuf UDP format (introduced in 1.5.0).
    pub version: (u16, u16, u16),
}

impl Default for GatewayConfig {
    fn default() -> GatewayConfig {
        GatewayConfig {
            // 64738 is Mumble's registered port; binding all interfaces is what
            // makes the demo reachable from another machine on the LAN.
            bind: SocketAddr::from(([0, 0, 0, 0], 64738)),
            welcome_text: String::new(),
            // 72 kbit/s is Murmur's default per-user bandwidth ceiling.
            max_bandwidth: 72_000,
            max_users: 100,
            recording_allowed: true,
            allow_html: true,
            message_length: 5_000,
            version: (1, 5, 0),
        }
    }
}

impl GatewayConfig {
    /// Encode the advertised version in the Mumble v2 format.
    ///
    /// REF: runtime/references/mumble/src/Version.h : `fromComponents` -
    ///   `version_v2 = (major << 48) | (minor << 32) | (patch << 16)`.
    #[must_use]
    pub fn version_v2(&self) -> u64 {
        let (major, minor, patch) = self.version;
        (u64::from(major) << 48) | (u64::from(minor) << 32) | (u64::from(patch) << 16)
    }
}
