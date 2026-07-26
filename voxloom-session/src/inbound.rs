//! Resolve client-supplied wire ids against one connection's committed view.
//!
//! A numeric channel id only has meaning inside the connection that received
//! it (ADR-007). Likewise, a process-wide session id is not authority to target
//! a user: that user must also be present in the sender's committed view. This
//! module is the single inbound boundary that enforces those two rules before a
//! client intent can reach server state (spec 20 invariants 13 and 14).
//!
//! Unsupported requests are still resolved first when they contain entity
//! references. That distinction matters for blobs and administration:
//! returning "unsupported" without ever consulting global state proves that a
//! guessed hidden id cannot become an existence oracle (invariants 16 and 17).

use thiserror::Error;
use voxloom_protocol::ControlMessage;
use voxloom_protocol::messages::tcp;
use voxloom_render::{ActionKey, ChannelId, ChannelKey, PermissionBits, SemanticKey, SessionId};

use crate::view::ConnectionView;

/// An inbound intent whose entity references have all been resolved in the
/// sender's committed view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundCommand {
    /// The only mutating intent supported by the deterministic Phase 6 server.
    MoveSelf { channel: ChannelKey },
    /// A read-only permission request for a visible channel.
    QueryPermissions {
        channel: ChannelKey,
        permissions: PermissionBits,
    },
    /// A structurally valid request that this phase deliberately does not
    /// implement. Its references were checked; the server may deny it without
    /// consulting any process-wide entity registry.
    ValidatedUnsupported { kind: UnsupportedKind },
}

/// Unsupported command families, kept explicit so adding support is an
/// auditable match rather than a stringly-typed exception.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedKind {
    TextMessage,
    VoiceTarget,
    ContextAction,
    Blob,
    Administration,
    UserState,
    ChannelMutation,
    PluginData,
}

/// Why an inbound message did not resolve to an authorized intent.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InboundError {
    #[error("channel id {0} is not visible in this connection's committed view")]
    InvisibleChannel(u32),
    #[error("session {0} is not visible in this connection's committed view")]
    InvisibleSession(u32),
    #[error("context action `{0}` is not visible in this connection's committed view")]
    InvisibleAction(String),
    #[error("message is malformed: {0}")]
    Malformed(&'static str),
    #[error("message kind has no supported inbound meaning in Phase 6")]
    UnsupportedMessage,
}

impl ConnectionView {
    /// Resolve every entity reference carried by `message` against this
    /// connection's committed view.
    ///
    /// The method never falls back to a global lookup. A guessed id therefore
    /// has exactly one outcome: [`InboundError::InvisibleChannel`] or
    /// [`InboundError::InvisibleSession`].
    pub fn resolve_inbound(
        &self,
        message: &ControlMessage,
    ) -> Result<InboundCommand, InboundError> {
        match message {
            ControlMessage::UserState(state) => self.resolve_user_state(state),
            ControlMessage::ChannelState(state) => {
                self.resolve_optional_channel(state.channel_id)?;
                self.resolve_optional_channel(state.parent)?;
                self.resolve_channels(&state.links)?;
                self.resolve_channels(&state.links_add)?;
                self.resolve_channels(&state.links_remove)?;
                Ok(unsupported(UnsupportedKind::ChannelMutation))
            }
            ControlMessage::ChannelRemove(remove) => {
                self.resolve_channel(remove.channel_id)?;
                Ok(unsupported(UnsupportedKind::ChannelMutation))
            }
            ControlMessage::UserRemove(remove) => {
                self.resolve_session(remove.session)?;
                self.resolve_optional_session(remove.actor)?;
                Ok(unsupported(UnsupportedKind::Administration))
            }
            ControlMessage::TextMessage(text) => {
                self.resolve_optional_session(text.actor)?;
                self.resolve_sessions(&text.session)?;
                self.resolve_channels(&text.channel_id)?;
                self.resolve_channels(&text.tree_id)?;
                Ok(unsupported(UnsupportedKind::TextMessage))
            }
            ControlMessage::Acl(acl) => {
                self.resolve_channel(acl.channel_id)?;
                Ok(unsupported(UnsupportedKind::Administration))
            }
            ControlMessage::ContextAction(action) => {
                self.resolve_optional_session(action.session)?;
                self.resolve_optional_channel(action.channel_id)?;
                self.resolve_action(&action.action)?;
                Ok(unsupported(UnsupportedKind::ContextAction))
            }
            ControlMessage::VoiceTarget(target) => {
                let id = target
                    .id
                    .ok_or(InboundError::Malformed("VoiceTarget.id is required"))?;
                if !(1..=30).contains(&id) {
                    return Err(InboundError::Malformed(
                        "VoiceTarget.id must be between 1 and 30",
                    ));
                }
                for entry in &target.targets {
                    self.resolve_sessions(&entry.session)?;
                    self.resolve_optional_channel(entry.channel_id)?;
                }
                Ok(unsupported(UnsupportedKind::VoiceTarget))
            }
            ControlMessage::PermissionQuery(query) => {
                let raw = query.channel_id.ok_or(InboundError::Malformed(
                    "PermissionQuery.channel_id is required",
                ))?;
                let channel = self.resolve_channel(raw)?;
                let permissions = self
                    .committed()
                    .permissions
                    .get(&ChannelId(raw))
                    .copied()
                    .unwrap_or(PermissionBits::NONE);
                Ok(InboundCommand::QueryPermissions {
                    channel,
                    permissions,
                })
            }
            ControlMessage::UserStats(stats) => {
                let session = stats
                    .session
                    .ok_or(InboundError::Malformed("UserStats.session is required"))?;
                self.resolve_session(session)?;
                Ok(unsupported(UnsupportedKind::Administration))
            }
            ControlMessage::RequestBlob(request) => {
                self.resolve_sessions(&request.session_texture)?;
                self.resolve_sessions(&request.session_comment)?;
                self.resolve_channels(&request.channel_description)?;
                Ok(unsupported(UnsupportedKind::Blob))
            }
            ControlMessage::PluginDataTransmission(plugin) => {
                self.resolve_optional_session(plugin.sender_session)?;
                self.resolve_sessions(&plugin.receiver_sessions)?;
                Ok(unsupported(UnsupportedKind::PluginData))
            }
            // These administration surfaces carry registered-user ids or whole
            // server datasets rather than view ids. They are denied without a
            // global lookup, which is precisely invariant 17's no-leak rule.
            ControlMessage::BanList(_)
            | ControlMessage::QueryUsers(_)
            | ControlMessage::UserList(_) => Ok(unsupported(UnsupportedKind::Administration)),

            // Ping, audio tunnel and crypto resync are handled by their own
            // protocol paths. Server-originated lifecycle messages have no
            // inbound command meaning.
            _ => Err(InboundError::UnsupportedMessage),
        }
    }

    fn resolve_user_state(&self, state: &tcp::UserState) -> Result<InboundCommand, InboundError> {
        if let Some(session) = state.session
            && session != self.self_session().0
        {
            // Even a visible peer is not a valid target for an ordinary
            // UserState: changing another user is an administrative action.
            return Err(InboundError::InvisibleSession(session));
        }
        self.resolve_optional_session(state.actor)?;
        self.resolve_channels(&state.listening_channel_add)?;
        self.resolve_channels(&state.listening_channel_remove)?;
        for adjustment in &state.listening_volume_adjustment {
            self.resolve_optional_channel(adjustment.listening_channel)?;
        }

        let Some(channel_id) = state.channel_id else {
            return Ok(unsupported(UnsupportedKind::UserState));
        };
        let channel = self.resolve_channel(channel_id)?;
        if !is_pure_self_move(state) {
            return Ok(unsupported(UnsupportedKind::UserState));
        }
        Ok(InboundCommand::MoveSelf { channel })
    }

    fn resolve_channel(&self, raw: u32) -> Result<ChannelKey, InboundError> {
        let id = ChannelId(raw);
        // Check the committed view as well as the mapping. `ids` may contain a
        // key allocated for a transition whose admission was refused; such an
        // id was never shown to the client and must not resolve inbound.
        if !self.committed().channels.contains_key(&id) {
            return Err(InboundError::InvisibleChannel(raw));
        }
        self.ids()
            .key_of(id)
            .cloned()
            .or_else(|| {
                (id == ChannelId::ROOT).then(|| {
                    self.committed()
                        .channels
                        .get(&ChannelId::ROOT)
                        .map(|channel| channel.key.clone())
                })?
            })
            .ok_or(InboundError::InvisibleChannel(raw))
    }

    fn resolve_optional_channel(
        &self,
        raw: Option<u32>,
    ) -> Result<Option<ChannelKey>, InboundError> {
        raw.map(|id| self.resolve_channel(id)).transpose()
    }

    fn resolve_channels(&self, raw: &[u32]) -> Result<Vec<ChannelKey>, InboundError> {
        raw.iter().map(|id| self.resolve_channel(*id)).collect()
    }

    fn resolve_session(&self, raw: u32) -> Result<SessionId, InboundError> {
        let id = SessionId(raw);
        self.committed()
            .users
            .contains_key(&id)
            .then_some(id)
            .ok_or(InboundError::InvisibleSession(raw))
    }

    fn resolve_optional_session(
        &self,
        raw: Option<u32>,
    ) -> Result<Option<SessionId>, InboundError> {
        raw.map(|id| self.resolve_session(id)).transpose()
    }

    fn resolve_sessions(&self, raw: &[u32]) -> Result<Vec<SessionId>, InboundError> {
        raw.iter().map(|id| self.resolve_session(*id)).collect()
    }

    fn resolve_action(&self, raw: &str) -> Result<ActionKey, InboundError> {
        self.committed()
            .context_actions
            .keys()
            .find(|key| wire_action_key(key) == raw)
            .cloned()
            .ok_or_else(|| InboundError::InvisibleAction(raw.to_owned()))
    }
}

fn unsupported(kind: UnsupportedKind) -> InboundCommand {
    InboundCommand::ValidatedUnsupported { kind }
}

fn wire_action_key(key: &ActionKey) -> String {
    match &key.0 {
        SemanticKey::Static(name) => format!("s:{name}"),
        SemanticKey::Dynamic(id) => format!("d:{id}"),
    }
}

/// A move request is intentionally narrow. Ignoring an extra mutation field
/// would turn an unsupported action into a silent success (R6).
fn is_pure_self_move(state: &tcp::UserState) -> bool {
    state.actor.is_none()
        && state.name.is_none()
        && state.user_id.is_none()
        && state.mute.is_none()
        && state.deaf.is_none()
        && state.suppress.is_none()
        && state.self_mute.is_none()
        && state.self_deaf.is_none()
        && state.texture.is_none()
        && state.plugin_context.is_none()
        && state.plugin_identity.is_none()
        && state.comment.is_none()
        && state.hash.is_none()
        && state.comment_hash.is_none()
        && state.texture_hash.is_none()
        && state.priority_speaker.is_none()
        && state.recording.is_none()
        && state.temporary_access_tokens.is_empty()
        && state.listening_channel_add.is_empty()
        && state.listening_channel_remove.is_empty()
        && state.listening_volume_adjustment.is_empty()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use voxloom_protocol::messages::tcp;
    use voxloom_reconcile::{AudioRoute, ChannelIdKind};
    use voxloom_render::{ClientView, SemanticKey, UserKey, ViewChannel, ViewUser};

    const SELF: SessionId = SessionId(7);
    const PEER: SessionId = SessionId(8);

    fn user(session: SessionId, name: &str, channel: ChannelId) -> ViewUser {
        ViewUser {
            key: UserKey(SemanticKey::Static(name.to_owned())),
            session,
            name: name.to_owned(),
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
        }
    }

    fn committed_connection() -> (ConnectionView, ChannelId, ChannelKey) {
        let mut connection = ConnectionView::new(SELF);
        let key = ChannelKey(SemanticKey::Static("realm:aurora".to_owned()));
        let id = connection
            .ids_mut()
            .resolve(key.clone(), ChannelIdKind::Stable)
            .expect("test id");

        let mut desired = ClientView::empty();
        desired.channels.insert(
            id,
            ViewChannel {
                key: key.clone(),
                id,
                parent: ChannelId::ROOT,
                name: "Aurora".to_owned(),
                description: None,
                position: 0,
                temporary: false,
                max_users: None,
                enter_restricted: false,
                can_enter: true,
                links: BTreeSet::new(),
            },
        );
        desired.users = BTreeMap::from([
            (SELF, user(SELF, "alice", id)),
            (PEER, user(PEER, "bob", id)),
        ]);
        desired
            .permissions
            .insert(id, PermissionBits(PermissionBits::ENTER));

        let pending = connection
            .prepare(&desired, &BTreeSet::<AudioRoute>::new())
            .expect("valid view")
            .expect("initial transition");
        let (_steps, token) = pending.split();
        connection.commit(token).expect("fresh token");
        (connection, id, key)
    }

    #[test]
    fn a_self_move_resolves_the_view_local_channel_id_to_its_key() {
        let (connection, id, key) = committed_connection();
        let command = connection
            .resolve_inbound(&ControlMessage::UserState(tcp::UserState {
                session: Some(SELF.0),
                channel_id: Some(id.0),
                ..Default::default()
            }))
            .expect("visible pure move");

        assert_eq!(command, InboundCommand::MoveSelf { channel: key });
    }

    #[test]
    fn an_id_allocated_but_never_committed_is_not_visible_inbound() {
        let (mut connection, _id, _key) = committed_connection();
        let hidden = connection
            .ids_mut()
            .resolve(
                ChannelKey(SemanticKey::Static("never-admitted".to_owned())),
                ChannelIdKind::Stable,
            )
            .expect("test id");

        let error = connection
            .resolve_inbound(&ControlMessage::PermissionQuery(tcp::PermissionQuery {
                channel_id: Some(hidden.0),
                ..Default::default()
            }))
            .expect_err("an allocated id is not proof it was shown");
        assert_eq!(error, InboundError::InvisibleChannel(hidden.0));
    }

    #[test]
    fn every_text_target_must_be_visible_to_the_sender() {
        let (connection, id, _key) = committed_connection();
        let error = connection
            .resolve_inbound(&ControlMessage::TextMessage(tcp::TextMessage {
                session: vec![PEER.0, 999],
                channel_id: vec![id.0],
                message: "probe".to_owned(),
                ..Default::default()
            }))
            .expect_err("hidden session must fail the whole command");
        assert_eq!(error, InboundError::InvisibleSession(999));
    }

    #[test]
    fn blob_requests_cannot_probe_hidden_entities() {
        let (connection, _id, _key) = committed_connection();
        let error = connection
            .resolve_inbound(&ControlMessage::RequestBlob(tcp::RequestBlob {
                session_texture: vec![999],
                ..Default::default()
            }))
            .expect_err("hidden blob owner must not resolve");
        assert_eq!(error, InboundError::InvisibleSession(999));
    }

    #[test]
    fn administration_is_denied_only_after_its_channel_is_resolved() {
        let (connection, id, _key) = committed_connection();
        assert_eq!(
            connection
                .resolve_inbound(&ControlMessage::Acl(tcp::Acl {
                    channel_id: id.0,
                    query: Some(true),
                    ..Default::default()
                }))
                .expect("visible channel, validated refusal"),
            unsupported(UnsupportedKind::Administration)
        );

        let error = connection
            .resolve_inbound(&ControlMessage::Acl(tcp::Acl {
                channel_id: 999,
                query: Some(true),
                ..Default::default()
            }))
            .expect_err("hidden channel must not reach administration");
        assert_eq!(error, InboundError::InvisibleChannel(999));
    }

    #[test]
    fn permission_queries_return_the_committed_effective_mask() {
        let (connection, id, key) = committed_connection();
        assert_eq!(
            connection
                .resolve_inbound(&ControlMessage::PermissionQuery(tcp::PermissionQuery {
                    channel_id: Some(id.0),
                    ..Default::default()
                }))
                .expect("visible permission query"),
            InboundCommand::QueryPermissions {
                channel: key,
                permissions: PermissionBits(PermissionBits::ENTER),
            }
        );
    }
}
