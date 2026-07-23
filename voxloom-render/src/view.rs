//! The normalized per-connection client view and its elements.
//!
//! REF: docs/voxloom-specification-technique-v0.1.md 8.3 (`ClientView`), 8.4
//!      (`ViewChannel`, `ViewUser`, `ServerPresentation`, listeners, context
//!      actions) and 19.1 (effective permission set).

use std::collections::{BTreeMap, BTreeSet};

use crate::ids::{ChannelId, SessionId};
use crate::keys::{ActionKey, ChannelKey, UserKey};

/// A reference to an out-of-band blob (channel description, user comment,
/// texture), served by hash only for visible entities (spec 20 invariant 16,
/// 21.2). Opaque here: the transport layer resolves the hash to bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobRef(pub String);

/// Effective permission mask for a connection on a channel (spec 19.1).
///
/// These are *canonical* view permission flags, already computed for the
/// connection. Bit positions are an internal, presentation-agnostic encoding in
/// the order of spec 19.1; they are NOT Mumble's ACL wire values. Mapping this
/// mask onto the `Permission` wire bits is the session layer's job (Phase 3),
/// which is why this crate must not know the wire encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct PermissionBits(pub u32);

impl PermissionBits {
    /// Empty mask (no permission granted).
    pub const NONE: PermissionBits = PermissionBits(0);

    // Flags in the declaration order of spec 19.1. Canonical, not wire bits.
    pub const WRITE: u32 = 1 << 0;
    pub const TRAVERSE: u32 = 1 << 1;
    pub const ENTER: u32 = 1 << 2;
    pub const SPEAK: u32 = 1 << 3;
    pub const MUTE_DEAFEN: u32 = 1 << 4;
    pub const MOVE: u32 = 1 << 5;
    pub const MAKE_CHANNEL: u32 = 1 << 6;
    pub const LINK_CHANNEL: u32 = 1 << 7;
    pub const WHISPER: u32 = 1 << 8;
    pub const TEXT_MESSAGE: u32 = 1 << 9;
    pub const MAKE_TEMP_CHANNEL: u32 = 1 << 10;
    pub const LISTEN: u32 = 1 << 11;
    pub const KICK: u32 = 1 << 12;
    pub const BAN: u32 = 1 << 13;
    pub const REGISTER: u32 = 1 << 14;
    pub const SELF_REGISTER: u32 = 1 << 15;
    pub const RESET_USER_CONTENT: u32 = 1 << 16;

    /// True if every flag in `flags` is granted.
    #[must_use]
    pub fn contains(self, flags: u32) -> bool {
        self.0 & flags == flags
    }
}

/// A channel as presented to one connection.
///
/// REF: docs/voxloom-specification-technique-v0.1.md 8.4 (`struct ViewChannel`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewChannel {
    /// Stable semantic identity across renders.
    pub key: ChannelKey,
    /// Per-connection numeric id (equals the map key in [`ClientView::channels`]).
    pub id: ChannelId,
    /// Parent channel; the root channel is its own parent.
    pub parent: ChannelId,
    pub name: String,
    pub description: Option<BlobRef>,
    pub position: i32,
    pub temporary: bool,
    pub max_users: Option<u32>,
    /// UI hint only: entering is restricted. Never a substitute for command
    /// validation (spec 19.2).
    pub enter_restricted: bool,
    /// UI hint only: the connection may enter (spec 19.2).
    pub can_enter: bool,
    pub links: BTreeSet<ChannelId>,
}

/// A user as presented to one connection.
///
/// REF: docs/voxloom-specification-technique-v0.1.md 8.4 (`struct ViewUser`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewUser {
    /// Stable semantic identity across renders.
    pub key: UserKey,
    /// Per-connection session id (equals the map key in [`ClientView::users`]).
    pub session: SessionId,
    pub name: String,
    /// Channel the user is shown in; must be a visible channel (spec 20 inv 5).
    pub channel: ChannelId,
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

/// Where a context action is offered (spec 8.4 `ContextAction`, 34 `ActionTarget`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ActionTarget {
    Server,
    Channel,
    User,
}

/// A context action shown in server, channel, or user menus (spec 8.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextActionView {
    pub key: ActionKey,
    pub target: ActionTarget,
    pub label: String,
}

/// A listener relation: a user listening to a channel. It is an interface-only
/// relation and is not required for real audio routing (spec 8.4, 21.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ListenerRelation {
    pub user: SessionId,
    pub channel: ChannelId,
}

/// Server-wide presentation parameters (spec 8.4 `ServerPresentation`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServerPresentation {
    pub welcome_text: Option<String>,
    pub allow_html: bool,
    pub max_message_length: Option<u32>,
    pub recording_allowed: bool,
}

/// The normalized view presented to a single connection.
///
/// Collections are ordered maps/sets so a given canonical state renders to a
/// byte-identical value (determinism is part of normalization, spec 12.3).
///
/// REF: docs/voxloom-specification-technique-v0.1.md 8.3 (`struct ClientView`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientView {
    pub root_channel: ChannelId,
    pub channels: BTreeMap<ChannelId, ViewChannel>,
    pub users: BTreeMap<SessionId, ViewUser>,
    pub listeners: BTreeSet<ListenerRelation>,
    pub permissions: BTreeMap<ChannelId, PermissionBits>,
    pub context_actions: BTreeMap<ActionKey, ContextActionView>,
    pub server_presentation: ServerPresentation,
}

impl ClientView {
    /// A view containing only the root channel and no users.
    ///
    /// This is the minimal structurally-valid view: the committed view of a
    /// connection starts here before any diff is applied.
    #[must_use]
    pub fn empty() -> ClientView {
        let root = ViewChannel {
            key: ChannelKey(crate::keys::SemanticKey::Static("root".to_owned())),
            id: ChannelId::ROOT,
            parent: ChannelId::ROOT,
            name: "Root".to_owned(),
            description: None,
            position: 0,
            temporary: false,
            max_users: None,
            enter_restricted: false,
            can_enter: true,
            links: BTreeSet::new(),
        };
        let mut channels = BTreeMap::new();
        channels.insert(ChannelId::ROOT, root);
        ClientView {
            root_channel: ChannelId::ROOT,
            channels,
            users: BTreeMap::new(),
            listeners: BTreeSet::new(),
            permissions: BTreeMap::new(),
            context_actions: BTreeMap::new(),
            server_presentation: ServerPresentation::default(),
        }
    }
}
