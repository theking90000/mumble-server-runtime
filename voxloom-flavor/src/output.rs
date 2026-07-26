//! Declarative outputs produced by a flavor before runtime-local id resolution.

use std::collections::{BTreeMap, BTreeSet};

use voxloom_render::{
    ActionKey, BlobRef, ChannelKey, ContextActionView, PermissionBits, ServerPresentation, UserKey,
};

use crate::ConnectionId;

/// A channel described only by stable semantic keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredChannel {
    pub key: ChannelKey,
    pub parent: ChannelKey,
    pub name: String,
    pub description: Option<BlobRef>,
    pub sort_order: i32,
    pub temporary: bool,
    pub max_users: Option<u32>,
    pub enter_restricted: bool,
    pub can_enter: bool,
    pub links: BTreeSet<ChannelKey>,
}

/// A visible user described without a runtime-local session id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredUser {
    pub key: UserKey,
    pub source_connection: Option<ConnectionId>,
    pub name: String,
    pub channel: ChannelKey,
    pub user_id: Option<u32>,
    pub certificate_hash: Option<String>,
    pub mute: bool,
    pub deaf: bool,
    pub suppress: bool,
    pub self_mute: bool,
    pub self_deaf: bool,
    pub priority_speaker: bool,
    pub recording: bool,
    pub comment: Option<BlobRef>,
    pub texture: Option<BlobRef>,
}

/// A visible listener relation expressed through semantic keys.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DesiredListenerRelation {
    pub user: UserKey,
    pub channel: ChannelKey,
}

/// A per-connection view before Voxloom assigns numeric view identifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredClientView {
    pub root_channel: ChannelKey,
    pub channels: BTreeMap<ChannelKey, DesiredChannel>,
    pub users: BTreeMap<UserKey, DesiredUser>,
    pub listeners: BTreeSet<DesiredListenerRelation>,
    pub permissions: BTreeMap<ChannelKey, PermissionBits>,
    pub context_actions: BTreeMap<ActionKey, ContextActionView>,
    pub server_presentation: ServerPresentation,
}

impl DesiredClientView {
    /// The smallest semantic view: one root and no users.
    #[must_use]
    pub fn empty() -> Self {
        let root = ChannelKey(voxloom_render::SemanticKey::Static("root".to_owned()));
        let root_channel = DesiredChannel {
            key: root.clone(),
            parent: root.clone(),
            name: "Root".to_owned(),
            description: None,
            sort_order: 0,
            temporary: false,
            max_users: None,
            enter_restricted: false,
            can_enter: true,
            links: BTreeSet::new(),
        };
        let channels = BTreeMap::from([(root.clone(), root_channel)]);

        Self {
            root_channel: root,
            channels,
            users: BTreeMap::new(),
            listeners: BTreeSet::new(),
            permissions: BTreeMap::new(),
            context_actions: BTreeMap::new(),
            server_presentation: ServerPresentation::default(),
        }
    }
}

/// One directional voice authorization before connection-to-session resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DesiredAudioRoute {
    pub sender: ConnectionId,
    pub receiver: ConnectionId,
}

/// Declarative voice outputs for one connection and one snapshot revision.
///
/// Construction goes through [`RenderOutput::new`] so later P7 tasks can extend
/// this contract without exposing its storage as part of the public API.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RenderOutput {
    client_view: DesiredClientView,
    audio_routes: BTreeSet<DesiredAudioRoute>,
}

impl RenderOutput {
    #[must_use]
    pub fn new(client_view: DesiredClientView, audio_routes: BTreeSet<DesiredAudioRoute>) -> Self {
        Self {
            client_view,
            audio_routes,
        }
    }

    #[must_use]
    pub fn client_view(&self) -> &DesiredClientView {
        &self.client_view
    }

    #[must_use]
    pub fn audio_routes(&self) -> &BTreeSet<DesiredAudioRoute> {
        &self.audio_routes
    }

    #[must_use]
    pub fn into_parts(self) -> (DesiredClientView, BTreeSet<DesiredAudioRoute>) {
        (self.client_view, self.audio_routes)
    }
}
