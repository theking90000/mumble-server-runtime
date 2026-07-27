//! The pure server->client handshake sequence.
//!
//! [`handshake_prelude`] and [`handshake_completion`] are the lifecycle frames
//! that bracket the connection's first published view: crypto and codec setup
//! before it, `ServerSync` and `ServerConfig` after it. They are pure functions
//! of their inputs (no IO, no randomness — the `CryptSetup` is passed in already
//! built). The view itself is the flavor's first generation, delivered between
//! the two by [`crate::connection`], which is what keeps §20 invariants 1 and 6
//! satisfied with no second rendering path.
//!
//! The order is authoritative, traced to Murmur (R1):
//! REF: references/mumble/src/murmur/Messages.cpp : `Server::msgAuthenticate` —
//!   CryptSetup, CodecVersion, ChannelState (root first, BFS parents before
//!   children), UserState(self), UserState(others), ServerSync, ServerConfig.
//! REF: references/mumble/src/murmur/Server.cpp : `Server::encrypted` — the
//!   server's own `Version` is sent right after TLS completes, BEFORE
//!   Authenticate; it is therefore [`server_version`], not part of this sequence.

use voxloom_protocol::ControlMessage;
use voxloom_protocol::messages::tcp;

use crate::config::ServerConfig;
use crate::state::SessionId;

/// Effective-permission bits, as the Mumble client understands them.
/// REF: references/mumble/src/ACL.h : `enum ChanACL::Perm`.
pub mod perm {
    pub const TRAVERSE: u32 = 0x2;
    pub const ENTER: u32 = 0x4;
    pub const SPEAK: u32 = 0x8;
    pub const WHISPER: u32 = 0x100;
    pub const TEXT_MESSAGE: u32 = 0x200;

    /// The permissions granted at the root channel in P3: be present and talk.
    /// Deliberately excludes channel/administration rights — those actions are
    /// refused server-side (spec §16.6/16.7), and the client should not offer
    /// their UI. Effective-permission computation proper is P4/P9.
    pub const ROOT_DEFAULT: u32 = TRAVERSE | ENTER | SPEAK | WHISPER | TEXT_MESSAGE;
}

/// The server's own `Version` message, sent immediately after the TLS handshake
/// completes and before the client's `Authenticate` (REF: `Server::encrypted`).
pub fn server_version(config: &ServerConfig) -> ControlMessage {
    let (major, minor, patch) = config.version;
    ControlMessage::Version(tcp::Version {
        // Legacy v1 field kept in sync for older introspection; the new v2 field
        // is what a 1.5+ client reads (REF Version.h: components are u16 each).
        version_v1: Some(
            (u32::from(major) << 16) | (u32::from(minor) << 8) | u32::from(patch.min(0xFF)),
        ),
        version_v2: Some(config.version_v2()),
        release: Some(format!("Voxloom {major}.{minor}.{patch}")),
        os: Some("Voxloom".to_string()),
        os_version: None,
    })
}

/// Lifecycle messages that precede the connection-specific view.
pub fn handshake_prelude(crypt_setup: tcp::CryptSetup) -> Vec<ControlMessage> {
    vec![
        // UDP crypto setup.
        ControlMessage::CryptSetup(crypt_setup),
        // Opus-only codec negotiation. REF Server.cpp: reset state is
        // `iCodecAlpha = iCodecBeta = 0; bPreferAlpha = false;`.
        ControlMessage::CodecVersion(tcp::CodecVersion {
            alpha: 0,
            beta: 0,
            prefer_alpha: false,
            opus: Some(true),
        }),
    ]
}

/// Lifecycle messages that complete the connection-specific view.
pub fn handshake_completion(config: &ServerConfig, self_session: SessionId) -> Vec<ControlMessage> {
    vec![
        // Synchronisation: the client learns its own session id here.
        ControlMessage::ServerSync(tcp::ServerSync {
            session: Some(self_session),
            max_bandwidth: Some(config.max_bandwidth),
            welcome_text: if config.welcome_text.is_empty() {
                None
            } else {
                Some(config.welcome_text.clone())
            },
            permissions: Some(u64::from(perm::ROOT_DEFAULT)),
        }),
        // Server configuration.
        ControlMessage::ServerConfig(tcp::ServerConfig {
            max_bandwidth: Some(config.max_bandwidth),
            welcome_text: None,
            allow_html: Some(config.allow_html),
            message_length: Some(config.message_length),
            image_message_length: None,
            max_users: Some(config.max_users),
            recording_allowed: Some(config.recording_allowed),
        }),
    ]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn dummy_crypt() -> tcp::CryptSetup {
        tcp::CryptSetup {
            key: Some(vec![0u8; 16]),
            client_nonce: Some(vec![0u8; 16]),
            server_nonce: Some(vec![0u8; 16]),
        }
    }

    #[test]
    fn the_prelude_sets_up_crypto_then_the_codec() {
        let messages = handshake_prelude(dummy_crypt());
        assert!(matches!(
            messages.first(),
            Some(ControlMessage::CryptSetup(_))
        ));
        assert!(matches!(
            messages.get(1),
            Some(ControlMessage::CodecVersion(_))
        ));
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn the_completion_syncs_before_it_configures() {
        let messages = handshake_completion(&ServerConfig::default(), 7);
        match messages.first() {
            // The client learns its own session here, after the view that
            // introduced it (§20 invariant 6, enforced end to end by the
            // simulated client against a live server).
            Some(ControlMessage::ServerSync(sync)) => assert_eq!(sync.session, Some(7)),
            other => panic!("expected ServerSync first, got {other:?}"),
        }
        assert!(matches!(
            messages.get(1),
            Some(ControlMessage::ServerConfig(_))
        ));
    }
}
