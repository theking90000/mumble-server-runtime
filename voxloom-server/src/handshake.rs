//! The pure server->client handshake sequence.
//!
//! [`build_handshake`] returns the ordered control messages a client receives
//! after it sends `Authenticate`. It is a pure function of its inputs (no IO, no
//! randomness — the `CryptSetup` is passed in already built), so the protocol
//! ordering invariants of spec §20 are unit-tested directly against it below.
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
use crate::state::{ChannelDef, ROOT_CHANNEL_ID, SessionId};

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

/// A connected peer as seen by the handshake (the "other users" already present).
#[derive(Debug, Clone)]
pub struct OtherUser {
    pub session: SessionId,
    pub name: String,
    pub channel_id: u32,
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

/// Build the ordered handshake emitted in response to `Authenticate`.
///
/// `crypt_setup` carries the freshly generated OCB2 key and nonces; it is built
/// by the connection (which owns the randomness) and passed in so this function
/// stays pure.
pub fn build_handshake(
    config: &ServerConfig,
    channels: &[ChannelDef],
    self_session: SessionId,
    self_name: &str,
    self_channel: u32,
    crypt_setup: tcp::CryptSetup,
    others: &[OtherUser],
) -> Vec<ControlMessage> {
    let mut out = handshake_prelude(crypt_setup);

    // 3. Channel tree, parents strictly before children (§20 invariants 3, 9).
    for channel in channel_emission_order(channels) {
        out.push(channel_state(channel));
    }

    // 4. Self user — must be known before ServerSync (§20 invariant 6).
    out.push(self_user_state(self_session, self_name, self_channel));

    // 5. Other visible users.
    for other in others {
        out.push(other_user_state(other));
    }

    out.extend(handshake_completion(config, self_session));
    out
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

/// Order channels so every channel appears after its parent (topological, root
/// first). Ties are broken by (position, id) for determinism. A channel whose
/// parent is missing from the set is emitted last, after everything reachable —
/// it can never precede a valid parent, so the parents-before-children invariant
/// holds for every well-formed tree.
fn channel_emission_order(channels: &[ChannelDef]) -> Vec<&ChannelDef> {
    let mut remaining: Vec<&ChannelDef> = channels.iter().collect();
    remaining.sort_by_key(|c| (c.position, c.id));

    let mut emitted: Vec<&ChannelDef> = Vec::with_capacity(remaining.len());
    let mut emitted_ids: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();

    // Repeatedly emit any channel whose parent is already emitted (the root is
    // its own parent and seeds the process). Bounded by channels.len() passes.
    let mut progressed = true;
    while progressed && emitted.len() < remaining.len() {
        progressed = false;
        for channel in &remaining {
            if emitted_ids.contains(&channel.id) {
                continue;
            }
            let parent_ready =
                channel.id == ROOT_CHANNEL_ID || emitted_ids.contains(&channel.parent);
            if parent_ready {
                emitted.push(channel);
                emitted_ids.insert(channel.id);
                progressed = true;
            }
        }
    }

    // Any channel left over has a missing/cyclic parent; append it so nothing is
    // silently dropped. Well-formed trees never reach this.
    for channel in &remaining {
        if !emitted_ids.contains(&channel.id) {
            emitted.push(channel);
        }
    }
    emitted
}

fn channel_state(channel: &ChannelDef) -> ControlMessage {
    // The root has no parent field (REF Messages.cpp: `if (c->cParent) ...`).
    let parent = if channel.id == ROOT_CHANNEL_ID {
        None
    } else {
        Some(channel.parent)
    };
    ControlMessage::ChannelState(tcp::ChannelState {
        channel_id: Some(channel.id),
        parent,
        name: Some(channel.name.clone()),
        position: Some(channel.position),
        ..Default::default()
    })
}

fn self_user_state(session: SessionId, name: &str, channel_id: u32) -> ControlMessage {
    // Self always carries channel_id (REF Messages.cpp: `mpus.set_channel_id`).
    ControlMessage::UserState(tcp::UserState {
        session: Some(session),
        name: Some(name.to_string()),
        channel_id: Some(channel_id),
        ..Default::default()
    })
}

fn other_user_state(other: &OtherUser) -> ControlMessage {
    // For other users Murmur omits channel_id when it is the root (0); an absent
    // channel_id means "root" to the client. REF Messages.cpp: `if (u->cChannel->iId != 0) mpus.set_channel_id(...)`.
    let channel_id = if other.channel_id == ROOT_CHANNEL_ID {
        None
    } else {
        Some(other.channel_id)
    };
    ControlMessage::UserState(tcp::UserState {
        session: Some(other.session),
        name: Some(other.name.clone()),
        channel_id,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn root_only() -> Vec<ChannelDef> {
        vec![ChannelDef {
            id: 0,
            parent: 0,
            name: "Root".to_string(),
            position: 0,
        }]
    }

    fn dummy_crypt() -> tcp::CryptSetup {
        tcp::CryptSetup {
            key: Some(vec![0u8; 16]),
            client_nonce: Some(vec![0u8; 16]),
            server_nonce: Some(vec![0u8; 16]),
        }
    }

    /// Index of the first message matching a predicate.
    fn position_of(msgs: &[ControlMessage], pred: impl Fn(&ControlMessage) -> bool) -> usize {
        msgs.iter()
            .position(pred)
            .expect("message must be present in the handshake")
    }

    #[test]
    fn handshake_order_matches_murmur() {
        let config = ServerConfig::default();
        let msgs = build_handshake(&config, &root_only(), 1, "alice", 0, dummy_crypt(), &[]);

        // The exact prefix ordering that the client relies on.
        assert!(matches!(msgs[0], ControlMessage::CryptSetup(_)));
        assert!(matches!(msgs[1], ControlMessage::CodecVersion(_)));
        assert!(matches!(msgs[2], ControlMessage::ChannelState(_)));

        let self_idx = position_of(&msgs, |m| matches!(m, ControlMessage::UserState(_)));
        let sync_idx = position_of(&msgs, |m| matches!(m, ControlMessage::ServerSync(_)));
        let config_idx = position_of(&msgs, |m| matches!(m, ControlMessage::ServerConfig(_)));

        // §20 invariant 6: the self-user is known before ServerSync.
        assert!(
            self_idx < sync_idx,
            "self UserState must precede ServerSync"
        );
        // ServerConfig comes after ServerSync (REF Messages.cpp).
        assert!(
            sync_idx < config_idx,
            "ServerSync must precede ServerConfig"
        );
    }

    #[test]
    fn server_sync_carries_the_session_id() {
        let config = ServerConfig::default();
        let msgs = build_handshake(&config, &root_only(), 7, "bob", 0, dummy_crypt(), &[]);
        match msgs
            .iter()
            .find(|m| matches!(m, ControlMessage::ServerSync(_)))
        {
            Some(ControlMessage::ServerSync(sync)) => assert_eq!(sync.session, Some(7)),
            _ => panic!("ServerSync missing"),
        }
    }

    #[test]
    fn self_user_state_precedes_other_users() {
        let config = ServerConfig::default();
        let others = [OtherUser {
            session: 1,
            name: "alice".to_string(),
            channel_id: 0,
        }];
        let msgs = build_handshake(&config, &root_only(), 2, "bob", 0, dummy_crypt(), &others);

        // The self UserState (session 2) must come before the other user's (1).
        let mut self_idx = None;
        let mut other_idx = None;
        for (i, m) in msgs.iter().enumerate() {
            if let ControlMessage::UserState(us) = m {
                if us.session == Some(2) {
                    self_idx = Some(i);
                } else if us.session == Some(1) {
                    other_idx = Some(i);
                }
            }
        }
        assert!(
            self_idx.expect("self present") < other_idx.expect("other present"),
            "self UserState must precede other users' UserState"
        );
    }

    #[test]
    fn channels_are_emitted_parents_before_children() {
        // A three-level tree given out of order; every parent must still precede
        // its children in the emitted ChannelState sequence (§20 invariants 3, 9).
        let channels = vec![
            ChannelDef {
                id: 2,
                parent: 1,
                name: "grandchild".to_string(),
                position: 0,
            },
            ChannelDef {
                id: 1,
                parent: 0,
                name: "child".to_string(),
                position: 0,
            },
            ChannelDef {
                id: 0,
                parent: 0,
                name: "Root".to_string(),
                position: 0,
            },
        ];
        let msgs = build_handshake(
            &ServerConfig::default(),
            &channels,
            1,
            "alice",
            0,
            dummy_crypt(),
            &[],
        );

        let mut order = Vec::new();
        for m in &msgs {
            if let ControlMessage::ChannelState(cs) = m {
                order.push((cs.channel_id, cs.parent));
            }
        }
        // Root first, then child (parent 0), then grandchild (parent 1).
        assert_eq!(
            order,
            vec![(Some(0), None), (Some(1), Some(0)), (Some(2), Some(1))]
        );
    }

    #[test]
    fn root_channel_has_no_parent_field() {
        let msgs = build_handshake(
            &ServerConfig::default(),
            &root_only(),
            1,
            "a",
            0,
            dummy_crypt(),
            &[],
        );
        match msgs
            .iter()
            .find(|m| matches!(m, ControlMessage::ChannelState(_)))
        {
            Some(ControlMessage::ChannelState(cs)) => {
                assert_eq!(cs.channel_id, Some(0));
                assert_eq!(cs.parent, None, "root must not declare a parent");
            }
            _ => panic!("root ChannelState missing"),
        }
    }
}
