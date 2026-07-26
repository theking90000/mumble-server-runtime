//! Turn client actions resolved in a committed view into voice events.
//!
//! An inbound message becomes an event only after its entity references have
//! been resolved in the sender's own committed view (spec 20 invariants 13 and
//! 14), and the event carries the generation that view belongs to. Anything
//! that does not resolve, or that this phase cannot express without inventing a
//! payload, is refused here rather than forwarded: a business command Voxloom
//! made up is exactly what the flavor boundary exists to prevent.
//!
//! REF: docs/voxloom-roadmap-agents-v0_1.md P7 T5
//! REF: docs/voxloom-specification-technique-v0.1.md 24.1, 24.4

use thiserror::Error;
use voxloom_flavor::{ConnectionId, VoiceEvent};
use voxloom_protocol::ControlMessage;
use voxloom_session::{InboundCommand, InboundError, UnsupportedKind};

use crate::PublicationCoordinator;

/// Why an inbound message produced no voice event.
///
/// Each variant is a refusal the caller must answer with a denial, never a
/// silent drop: a client that gets no answer retries the same action forever.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum VoiceEventError {
    #[error("connection {connection:?} is not registered")]
    UnknownConnection { connection: ConnectionId },
    #[error("connection {connection:?} referenced an entity it cannot see: {source}")]
    Unresolved {
        connection: ConnectionId,
        #[source]
        source: InboundError,
    },
    #[error(
        "connection {connection:?} sent a {kind:?} interaction, which carries no voice event in \
         this phase"
    )]
    UnsupportedInteraction {
        connection: ConnectionId,
        kind: UnsupportedKind,
    },
}

impl PublicationCoordinator {
    /// The event announcing that `connection` holds a committed view.
    #[must_use]
    pub const fn connected(&self, connection: ConnectionId) -> VoiceEvent {
        VoiceEvent::Connected {
            connection,
            generation: self.generation(),
        }
    }

    /// The event announcing that `connection` was refused before it held a view.
    #[must_use]
    pub fn authentication_failed(
        &self,
        connection: ConnectionId,
        reason: impl Into<String>,
    ) -> VoiceEvent {
        VoiceEvent::AuthenticationFailed {
            connection,
            generation: self.generation(),
            reason: reason.into(),
        }
    }

    /// The event announcing that `connection` is gone.
    #[must_use]
    pub fn disconnected(&self, connection: ConnectionId, reason: impl Into<String>) -> VoiceEvent {
        VoiceEvent::Disconnected {
            connection,
            generation: self.generation(),
            reason: reason.into(),
        }
    }

    /// Resolve one inbound message into the event the flavor should observe.
    ///
    /// `Ok(None)` means the message resolved but has no business meaning: a
    /// permission query is answered by the runtime from the committed view the
    /// flavor already produced, so forwarding it would ask the flavor to
    /// re-decide something it has already decided.
    pub fn resolve_event(
        &self,
        connection: ConnectionId,
        message: &ControlMessage,
    ) -> Result<Option<VoiceEvent>, VoiceEventError> {
        let view = self
            .connection_view(connection)
            .ok_or(VoiceEventError::UnknownConnection { connection })?;
        let command = view
            .resolve_inbound(message)
            .map_err(|source| VoiceEventError::Unresolved { connection, source })?;

        match command {
            InboundCommand::MoveSelf { channel } => {
                Ok(Some(VoiceEvent::ChannelInteractionRequested {
                    connection,
                    generation: self.generation(),
                    channel,
                }))
            }
            InboundCommand::QueryPermissions { .. } => Ok(None),
            // Context actions, text and voice targets resolve their references
            // but their payload is deliberately dropped by the resolver. Making
            // an event out of that would mean inventing the part the client
            // actually sent; they stay refused until Phase 9 defines them.
            InboundCommand::ValidatedUnsupported { kind } => {
                Err(VoiceEventError::UnsupportedInteraction { connection, kind })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Mutex;
    use std::sync::{Arc, atomic::AtomicUsize, atomic::Ordering};

    use voxloom_flavor::{
        ChannelKey, DesiredChannel, DesiredClientView, DesiredUser, FlavorError, FlavorRevision,
        InteractionRegistry, RenderOutput, SemanticKey, UserKey, VoiceFlavor,
    };
    use voxloom_protocol::messages::tcp;
    use voxloom_render::SessionId;

    use super::*;
    use crate::{render_snapshot, validate_rendered_snapshot};

    const CONNECTION: ConnectionId = ConnectionId::new(4);
    const SESSION: SessionId = SessionId(40);

    fn realm_key() -> ChannelKey {
        ChannelKey(SemanticKey::Static("realm:aurora".to_owned()))
    }

    /// A one-connection flavor with a second channel to interact with, and a
    /// record of everything Voxloom reported to it.
    #[derive(Debug, Default)]
    struct RecordingFlavor {
        observed: Mutex<Vec<VoiceEvent>>,
        renders: AtomicUsize,
    }

    impl VoiceFlavor for RecordingFlavor {
        type Snapshot = FlavorRevision;

        fn revision(&self, snapshot: &Self::Snapshot) -> FlavorRevision {
            *snapshot
        }

        fn render(
            &self,
            _snapshot: &Self::Snapshot,
            connection: ConnectionId,
        ) -> Result<RenderOutput, FlavorError> {
            self.renders.fetch_add(1, Ordering::Relaxed);
            let mut view = DesiredClientView::empty();
            let realm = realm_key();
            view.channels.insert(
                realm.clone(),
                DesiredChannel {
                    key: realm.clone(),
                    parent: view.root_channel.clone(),
                    name: "Aurora".to_owned(),
                    description: None,
                    sort_order: 0,
                    temporary: false,
                    max_users: None,
                    enter_restricted: false,
                    can_enter: true,
                    links: BTreeSet::new(),
                },
            );
            let key = UserKey(SemanticKey::Dynamic(connection.get()));
            view.users.insert(
                key.clone(),
                DesiredUser {
                    key,
                    source_connection: Some(connection),
                    name: "alice".to_owned(),
                    channel: view.root_channel.clone(),
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
            Ok(RenderOutput::new(
                view,
                BTreeSet::new(),
                InteractionRegistry::default(),
            ))
        }

        fn observe(&self, event: &VoiceEvent) {
            match self.observed.lock() {
                Ok(mut observed) => observed.push(event.clone()),
                Err(poisoned) => poisoned.into_inner().push(event.clone()),
            }
        }
    }

    /// A coordinator holding one connection at generation 1, and the flavor it
    /// was rendered from.
    fn committed() -> (PublicationCoordinator, RecordingFlavor) {
        let flavor = RecordingFlavor::default();
        let mut coordinator = PublicationCoordinator::new();
        match coordinator.register(CONNECTION, SESSION) {
            Ok(()) => {}
            Err(error) => panic!("registration failed: {error}"),
        }
        let rendered =
            match render_snapshot(&flavor, Arc::new(FlavorRevision::new(1)), [CONNECTION]) {
                Ok(rendered) => rendered,
                Err(error) => panic!("render failed: {error}"),
            };
        let validated = match validate_rendered_snapshot(rendered) {
            Ok(validated) => validated,
            Err(error) => panic!("validation failed: {error}"),
        };
        let pending = match coordinator.publish(&validated) {
            Ok(pending) => pending,
            Err(error) => panic!("publication failed: {error}"),
        };
        let (_deliveries, commit) = pending.split();
        match coordinator.commit(commit) {
            Ok(_published) => {}
            Err(error) => panic!("commit failed: {error}"),
        }
        (coordinator, flavor)
    }

    /// The numeric id the coordinator gave a semantic channel key.
    fn committed_channel_id(coordinator: &PublicationCoordinator, key: &ChannelKey) -> u32 {
        match coordinator.committed_view(CONNECTION) {
            Some(view) => match view.channels.values().find(|channel| &channel.key == key) {
                Some(channel) => channel.id.0,
                None => panic!("channel {key:?} is not committed"),
            },
            None => panic!("connection is not registered"),
        }
    }

    #[test]
    fn a_self_move_becomes_an_interaction_request_stamped_with_the_generation() {
        let (coordinator, flavor) = committed();
        let channel_id = committed_channel_id(&coordinator, &realm_key());
        let renders_before = flavor.renders.load(Ordering::Relaxed);

        let event = coordinator.resolve_event(
            CONNECTION,
            &ControlMessage::UserState(tcp::UserState {
                session: Some(SESSION.0),
                channel_id: Some(channel_id),
                ..Default::default()
            }),
        );

        match event {
            Ok(Some(event)) => {
                assert_eq!(
                    event,
                    VoiceEvent::ChannelInteractionRequested {
                        connection: CONNECTION,
                        generation: 1,
                        channel: realm_key(),
                    }
                );
                flavor.observe(&event);
            }
            Ok(None) => panic!("a self move must reach the flavor"),
            Err(error) => panic!("resolution failed: {error}"),
        }

        // Voxloom reported the request and did nothing else: no view moved, no
        // new generation, and no render was triggered behind the flavor's back.
        assert_eq!(coordinator.generation(), 1);
        assert_eq!(flavor.renders.load(Ordering::Relaxed), renders_before);
        match coordinator.committed_view(CONNECTION) {
            Some(view) => match view.users.get(&SESSION) {
                Some(user) => assert_eq!(user.channel.0, 0, "the user is still in the root"),
                None => panic!("the connection lost its own user"),
            },
            None => panic!("connection is not registered"),
        }
        match flavor.observed.lock() {
            Ok(observed) => assert_eq!(observed.len(), 1),
            Err(_) => panic!("the flavor's record is poisoned"),
        }
    }

    #[test]
    fn a_permission_query_is_answered_by_the_runtime_and_never_reaches_the_flavor() {
        let (coordinator, _flavor) = committed();
        let channel_id = committed_channel_id(&coordinator, &realm_key());

        let event = coordinator.resolve_event(
            CONNECTION,
            &ControlMessage::PermissionQuery(tcp::PermissionQuery {
                channel_id: Some(channel_id),
                ..Default::default()
            }),
        );

        match event {
            Ok(None) => {}
            Ok(Some(event)) => panic!("a read-only query became an event: {event:?}"),
            Err(error) => panic!("resolution failed: {error}"),
        }
    }

    #[test]
    fn an_unsupported_interaction_is_refused_instead_of_invented() {
        let (coordinator, _flavor) = committed();

        let event = coordinator.resolve_event(
            CONNECTION,
            &ControlMessage::TextMessage(tcp::TextMessage {
                message: "hello".to_owned(),
                ..Default::default()
            }),
        );

        match event {
            Err(VoiceEventError::UnsupportedInteraction { connection, kind }) => {
                assert_eq!(connection, CONNECTION);
                assert_eq!(kind, UnsupportedKind::TextMessage);
            }
            Err(error) => panic!("unexpected error: {error}"),
            Ok(event) => panic!("an unsupported interaction produced {event:?}"),
        }
    }

    #[test]
    fn an_invisible_channel_never_becomes_an_event() {
        let (coordinator, _flavor) = committed();

        let event = coordinator.resolve_event(
            CONNECTION,
            &ControlMessage::UserState(tcp::UserState {
                session: Some(SESSION.0),
                channel_id: Some(4_242),
                ..Default::default()
            }),
        );

        match event {
            Err(VoiceEventError::Unresolved { connection, source }) => {
                assert_eq!(connection, CONNECTION);
                assert_eq!(source, InboundError::InvisibleChannel(4_242));
            }
            Err(error) => panic!("unexpected error: {error}"),
            Ok(event) => panic!("a guessed id produced {event:?}"),
        }
    }

    #[test]
    fn an_unregistered_connection_produces_no_event() {
        let (coordinator, _flavor) = committed();
        let stranger = ConnectionId::new(99);

        let event = coordinator.resolve_event(
            stranger,
            &ControlMessage::UserState(tcp::UserState {
                session: Some(SESSION.0),
                channel_id: Some(0),
                ..Default::default()
            }),
        );

        assert_eq!(
            event.err(),
            Some(VoiceEventError::UnknownConnection {
                connection: stranger
            })
        );
    }

    #[test]
    fn lifecycle_events_carry_the_current_generation() {
        let (coordinator, _flavor) = committed();

        assert_eq!(
            coordinator.connected(CONNECTION),
            VoiceEvent::Connected {
                connection: CONNECTION,
                generation: 1,
            }
        );
        assert_eq!(
            coordinator.disconnected(CONNECTION, "transport closed"),
            VoiceEvent::Disconnected {
                connection: CONNECTION,
                generation: 1,
                reason: "transport closed".to_owned(),
            }
        );
        assert_eq!(
            coordinator
                .authentication_failed(ConnectionId::new(7), "certificate rejected")
                .connection(),
            ConnectionId::new(7)
        );
    }
}
