//! Deterministic live-view scenario for Phase 6.
//!
//! This is intentionally server-local demo state, not the canonical business
//! state of Phase 7. It supplies the smallest vertical scenario that exercises
//! the real per-connection renderer:
//!
//! - two synthetic realms, Aurora and Borealis;
//! - viewer-relative channel labels, so two viewers in different realms hold
//!   observably different trees;
//! - only users in the viewer's realm are visible;
//! - moving self to either visible channel changes realm without reconnecting;
//! - audio routes are directional and derived from the same visibility answer.
//!
//! Usernames ending in `@aurora` or `@borealis` select the initial realm and
//! have the suffix removed from the displayed name. All other names retain the
//! Phase 3 behaviour and start in Aurora, preserving the existing basic server
//! scenario.

use std::collections::{BTreeMap, BTreeSet};

use voxloom_reconcile::{AudioRoute, ChannelIdKind, IdError};
use voxloom_render::{
    ChannelId, ChannelKey, ClientView, PermissionBits, SemanticKey, ServerPresentation, SessionId,
    UserKey, ViewChannel, ViewUser,
};
use voxloom_session::ConnectionView;

use crate::config::ServerConfig;

const AURORA_KEY: &str = "realm:aurora";
const BOREALIS_KEY: &str = "realm:borealis";

/// The routing partition used by the deterministic P6 scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Realm {
    Aurora,
    Borealis,
}

impl Realm {
    pub const ALL: [Realm; 2] = [Realm::Aurora, Realm::Borealis];

    pub const fn routing_id(self) -> u32 {
        match self {
            Realm::Aurora => 0,
            Realm::Borealis => 1,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Realm::Aurora => "Aurora",
            Realm::Borealis => "Borealis",
        }
    }

    fn key(self) -> ChannelKey {
        ChannelKey(SemanticKey::Static(
            match self {
                Realm::Aurora => AURORA_KEY,
                Realm::Borealis => BOREALIS_KEY,
            }
            .to_owned(),
        ))
    }
}

/// The projection-relevant facts copied out of a live connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioUser {
    pub session: u32,
    pub name: String,
    pub realm: Realm,
}

/// Parse the optional deterministic-scenario suffix.
pub fn scenario_identity(raw: &str) -> (String, Realm) {
    let lower = raw.to_ascii_lowercase();
    let (display, realm) = if lower.ends_with("@borealis") {
        (&raw[..raw.len() - "@borealis".len()], Realm::Borealis)
    } else if lower.ends_with("@aurora") {
        (&raw[..raw.len() - "@aurora".len()], Realm::Aurora)
    } else {
        (raw, Realm::Aurora)
    };
    let display = display.trim();
    (
        if display.is_empty() {
            "Guest".to_owned()
        } else {
            display.chars().take(64).collect()
        },
        realm,
    )
}

/// Convert a resolved semantic channel key into the scenario realm it denotes.
pub fn realm_from_channel_key(key: &ChannelKey) -> Option<Realm> {
    match &key.0 {
        SemanticKey::Static(value) if value == AURORA_KEY => Some(Realm::Aurora),
        SemanticKey::Static(value) if value == BOREALIS_KEY => Some(Realm::Borealis),
        _ => None,
    }
}

/// Render one connection's desired view and incoming audio routes.
pub fn render(
    connection: &mut ConnectionView,
    viewer: &ScenarioUser,
    users: &[ScenarioUser],
    config: &ServerConfig,
) -> Result<(ClientView, BTreeSet<AudioRoute>), IdError> {
    let mut channels = BTreeMap::new();
    let mut root = ClientView::empty()
        .channels
        .remove(&ChannelId::ROOT)
        .unwrap_or_else(|| unreachable!("ClientView::empty always contains root"));
    root.name = config.server_name.clone();
    channels.insert(ChannelId::ROOT, root);

    let mut channel_ids = BTreeMap::new();
    for (position, realm) in Realm::ALL.into_iter().enumerate() {
        let key = realm.key();
        let id = connection
            .ids_mut()
            .resolve(key.clone(), ChannelIdKind::Stable)?;
        channel_ids.insert(realm, id);
        let relation = if realm == viewer.realm {
            "Your realm"
        } else {
            "Switch to"
        };
        channels.insert(
            id,
            ViewChannel {
                key,
                id,
                parent: ChannelId::ROOT,
                name: format!("{relation} · {}", realm.label()),
                description: None,
                position: i32::try_from(position).unwrap_or(0),
                temporary: false,
                max_users: None,
                enter_restricted: false,
                can_enter: true,
                links: BTreeSet::new(),
            },
        );
    }

    let viewer_channel = channel_ids
        .get(&viewer.realm)
        .copied()
        .ok_or(IdError::StableRangeExhausted)?;
    let mut view_users = BTreeMap::new();
    for user in users.iter().filter(|user| user.realm == viewer.realm) {
        let channel = channel_ids
            .get(&user.realm)
            .copied()
            .unwrap_or(viewer_channel);
        let session = SessionId(user.session);
        view_users.insert(
            session,
            ViewUser {
                key: UserKey(SemanticKey::Dynamic(u64::from(user.session))),
                session,
                name: user.name.clone(),
                channel,
                user_id: None,
                certificate_hash: None,
                mute: false,
                deaf: false,
                suppress: false,
                self_mute: false,
                self_deaf: false,
                priority_speaker: false,
                recording: false,
                comment: None,
                texture: None,
            },
        );
    }

    let effective = PermissionBits(
        PermissionBits::TRAVERSE
            | PermissionBits::ENTER
            | PermissionBits::SPEAK
            | PermissionBits::WHISPER
            | PermissionBits::TEXT_MESSAGE,
    );
    let permissions = channels.keys().copied().map(|id| (id, effective)).collect();

    let desired = ClientView {
        root_channel: ChannelId::ROOT,
        channels,
        users: view_users,
        listeners: BTreeSet::new(),
        permissions,
        context_actions: BTreeMap::new(),
        server_presentation: ServerPresentation {
            welcome_text: (!config.welcome_text.is_empty()).then(|| config.welcome_text.clone()),
            allow_html: config.allow_html,
            max_message_length: Some(config.message_length),
            recording_allowed: config.recording_allowed,
        },
    };

    // A receiver may hear exactly the speakers its committed view knows. The
    // self route is omitted for normal speech; server loopback is handled by the
    // router's dedicated `just_sender` path.
    let routes = users
        .iter()
        .filter(|sender| sender.realm == viewer.realm && sender.session != viewer.session)
        .map(|sender| AudioRoute {
            sender: SessionId(sender.session),
            receiver: SessionId(viewer.session),
        })
        .collect();

    Ok((desired, routes))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn suffixes_select_realms_without_changing_display_identity() {
        assert_eq!(
            scenario_identity("alice@aurora"),
            ("alice".to_owned(), Realm::Aurora)
        );
        assert_eq!(
            scenario_identity("bob@BOREALIS"),
            ("bob".to_owned(), Realm::Borealis)
        );
        assert_eq!(
            scenario_identity("carol"),
            ("carol".to_owned(), Realm::Aurora)
        );
    }

    #[test]
    fn viewers_in_different_realms_get_different_trees_and_users() {
        let users = vec![
            ScenarioUser {
                session: 1,
                name: "alice".to_owned(),
                realm: Realm::Aurora,
            },
            ScenarioUser {
                session: 2,
                name: "bob".to_owned(),
                realm: Realm::Borealis,
            },
        ];
        let config = ServerConfig::default();
        let mut alice = ConnectionView::new(SessionId(1));
        let mut bob = ConnectionView::new(SessionId(2));

        let (alice_view, alice_routes) =
            render(&mut alice, &users[0], &users, &config).expect("alice view");
        let (bob_view, bob_routes) =
            render(&mut bob, &users[1], &users, &config).expect("bob view");

        assert_ne!(alice_view, bob_view, "viewer-relative trees must diverge");
        assert_eq!(
            alice_view.users.keys().copied().collect::<Vec<_>>(),
            vec![SessionId(1)]
        );
        assert_eq!(
            bob_view.users.keys().copied().collect::<Vec<_>>(),
            vec![SessionId(2)]
        );
        assert!(alice_routes.is_empty());
        assert!(bob_routes.is_empty());
    }

    #[test]
    fn same_realm_visibility_produces_directional_routes() {
        let users = vec![
            ScenarioUser {
                session: 1,
                name: "alice".to_owned(),
                realm: Realm::Aurora,
            },
            ScenarioUser {
                session: 2,
                name: "bob".to_owned(),
                realm: Realm::Aurora,
            },
        ];
        let mut alice = ConnectionView::new(SessionId(1));
        let (_view, routes) =
            render(&mut alice, &users[0], &users, &ServerConfig::default()).expect("view");

        assert_eq!(
            routes,
            BTreeSet::from([AudioRoute {
                sender: SessionId(2),
                receiver: SessionId(1),
            }])
        );
    }
}
