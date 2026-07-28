//! The pure server-to-client handshake sequence.
//!
//! [`prelude`] and [`completion`] are the lifecycle frames that bracket the
//! connection's first view: crypto and codec setup before it, `ServerSync` and
//! `ServerConfig` after it. The view in between is whatever the shard pushed
//! onto this connection's queue, so there is exactly one rendering path in the
//! runtime and the handshake is not a second one.
//!
//! The order is authoritative, traced to Murmur (R1):
//! REF: references/mumble/src/murmur/Messages.cpp : `Server::msgAuthenticate` -
//!   CryptSetup, CodecVersion, ChannelState (root first, parents before
//!   children), UserState(self), UserState(others), ServerSync, ServerConfig.
//! REF: references/mumble/src/murmur/Server.cpp : `Server::encrypted` - the
//!   server's own `Version` goes out as soon as TLS completes, BEFORE
//!   Authenticate; it is therefore [`server_version`] and not part of this
//!   sequence.

use mumble_server_runtime_protocol::ControlMessage;
use mumble_server_runtime_protocol::messages::tcp;
use voxloom_shard::SessionId;

use crate::config::GatewayConfig;

/// Effective-permission bits, as the Mumble client understands them.
///
/// Re-exported rather than restated: the shard answers `PermissionQuery` from
/// the same bits, and two definitions of one constant is one definition too
/// many.
pub use voxloom_shard::perm;

/// The server's own `Version`, sent immediately after the TLS handshake
/// completes and before the client's `Authenticate`.
#[must_use]
pub fn server_version(config: &GatewayConfig) -> ControlMessage {
    let (major, minor, patch) = config.version;
    ControlMessage::Version(tcp::Version {
        // The legacy v1 field stays in sync for older introspection; a 1.5+
        // client reads v2 (REF Version.h: each component is a u16).
        version_v1: Some(
            (u32::from(major) << 16) | (u32::from(minor) << 8) | u32::from(patch.min(0xFF)),
        ),
        version_v2: Some(config.version_v2()),
        release: Some(format!("Mumble Server Runtime {major}.{minor}.{patch}")),
        os: Some("Mumble Server Runtime".to_owned()),
        os_version: None,
    })
}

/// Lifecycle messages that precede the connection's first view.
#[must_use]
pub fn prelude(crypt_setup: tcp::CryptSetup) -> Vec<ControlMessage> {
    vec![
        ControlMessage::CryptSetup(crypt_setup),
        // Opus only. REF Server.cpp: the reset state is
        // `iCodecAlpha = iCodecBeta = 0; bPreferAlpha = false;`.
        ControlMessage::CodecVersion(tcp::CodecVersion {
            alpha: 0,
            beta: 0,
            prefer_alpha: false,
            opus: Some(true),
        }),
    ]
}

/// Lifecycle messages that complete it.
#[must_use]
pub fn completion(config: &GatewayConfig, session: SessionId) -> Vec<ControlMessage> {
    vec![
        // The client learns its own session here, after the view that
        // introduced it (invariant 6).
        ControlMessage::ServerSync(tcp::ServerSync {
            session: Some(session.0),
            max_bandwidth: Some(config.max_bandwidth),
            welcome_text: if config.welcome_text.is_empty() {
                None
            } else {
                Some(config.welcome_text.clone())
            },
            permissions: Some(u64::from(perm::DEFAULT)),
        }),
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

/// Refuse a connection before it is attached to anything.
///
/// REF: references/vendored/Mumble.proto : `Reject.RejectType`.
#[must_use]
pub fn reject(reason: &str) -> ControlMessage {
    ControlMessage::Reject(tcp::Reject {
        r#type: Some(i32::from(tcp::reject::RejectType::None)),
        reason: Some(reason.to_owned()),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn crypt() -> tcp::CryptSetup {
        tcp::CryptSetup {
            key: Some(vec![0u8; 16]),
            client_nonce: Some(vec![0u8; 16]),
            server_nonce: Some(vec![0u8; 16]),
        }
    }

    #[test]
    fn the_prelude_sets_up_crypto_then_the_codec() {
        let messages = prelude(crypt());
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
        let messages = completion(&GatewayConfig::default(), SessionId(7));
        match messages.first() {
            Some(ControlMessage::ServerSync(sync)) => assert_eq!(sync.session, Some(7)),
            other => panic!("expected ServerSync first, got {other:?}"),
        }
        assert!(matches!(
            messages.get(1),
            Some(ControlMessage::ServerConfig(_))
        ));
    }
}
