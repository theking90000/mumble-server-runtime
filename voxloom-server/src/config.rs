//! Static server configuration for the minimal Phase 3 server.
//!
//! These are the values the handshake advertises (`ServerConfig`, `ServerSync`,
//! `Version`, welcome text). They are fixed for the lifetime of the process;
//! dynamic reconfiguration is out of scope for P3.

/// Server-wide configuration advertised during the handshake.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Human-readable server/root-channel name, shown by the client.
    pub server_name: String,
    /// Welcome text sent in `ServerSync`. Empty means "omit the field".
    pub welcome_text: String,
    /// Advertised maximum bandwidth (bits/s), echoed to the client in
    /// `ServerSync` and `ServerConfig`.
    pub max_bandwidth: u32,
    /// Advertised maximum number of users (`ServerConfig.max_users`).
    pub max_users: u32,
    /// Whether the client's built-in recording feature is announced as allowed
    /// (`ServerConfig.recording_allowed`). This is advisory only (spec §21.8).
    pub recording_allowed: bool,
    /// Whether HTML is allowed in text (`ServerConfig.allow_html`).
    pub allow_html: bool,
    /// Maximum text-message length (`ServerConfig.message_length`).
    pub message_length: u32,
    /// Advertised server version (major, minor, patch). Defaults to 1.5.0 so a
    /// real client negotiates the protobuf UDP format (introduced in 1.5.0).
    pub version: (u16, u16, u16),
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            server_name: "Voxloom".to_string(),
            welcome_text: "Welcome to Voxloom".to_string(),
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

impl ServerConfig {
    /// Encode the advertised version in the Mumble v2 format.
    ///
    /// REF: references/mumble/src/Version.h : `fromComponents` —
    /// `version_v2 = (major << 48) | (minor << 32) | (patch << 16)`.
    pub fn version_v2(&self) -> u64 {
        let (major, minor, patch) = self.version;
        (u64::from(major) << 48) | (u64::from(minor) << 32) | (u64::from(patch) << 16)
    }
}
