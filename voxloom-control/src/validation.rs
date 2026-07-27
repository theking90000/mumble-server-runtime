//! Pure validation of a complete flavor render before publication.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use thiserror::Error;
use voxloom_flavor::{
    ActionKey, ChannelKey, ConnectionId, DesiredAudioRoute, DesiredClientView, FlavorRevision,
    RenderOutput, UserKey,
};

use crate::RenderedSnapshot;

/// A complete flavor render that passed every P7 validation pass.
///
/// Construction is intentionally restricted to [`validate_rendered_snapshot`].
/// Later publication stages can require this type and remain unable to consume
/// unchecked flavor output.
#[derive(Debug)]
#[must_use = "a validated snapshot has no effect until a later publication stage consumes it"]
pub struct ValidatedSnapshot<S> {
    rendered: RenderedSnapshot<S>,
}

impl<S> ValidatedSnapshot<S> {
    #[must_use]
    pub fn snapshot(&self) -> &Arc<S> {
        self.rendered.snapshot()
    }

    #[must_use]
    pub const fn flavor_revision(&self) -> FlavorRevision {
        self.rendered.flavor_revision()
    }

    #[must_use]
    pub fn outputs(&self) -> &BTreeMap<ConnectionId, RenderOutput> {
        self.rendered.outputs()
    }

    pub fn into_rendered(self) -> RenderedSnapshot<S> {
        self.rendered
    }
}

/// Static semantic-view failures detected before runtime-local id assignment.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum DesiredViewValidationError {
    #[error("channel map key {map_key:?} differs from declared key {declared_key:?}")]
    ChannelKeyMismatch {
        map_key: ChannelKey,
        declared_key: ChannelKey,
    },
    #[error("root channel {root:?} is absent")]
    RootChannelMissing { root: ChannelKey },
    #[error("root channel {root:?} declares parent {parent:?}")]
    RootParentMismatch {
        root: ChannelKey,
        parent: ChannelKey,
    },
    #[error("channel {channel:?} references absent parent {parent:?}")]
    MissingParent {
        channel: ChannelKey,
        parent: ChannelKey,
    },
    #[error("parent chain from {channel:?} cycles at {repeated:?}")]
    ParentCycle {
        channel: ChannelKey,
        repeated: ChannelKey,
    },
    #[error("user map key {map_key:?} differs from declared key {declared_key:?}")]
    UserKeyMismatch {
        map_key: UserKey,
        declared_key: UserKey,
    },
    #[error("user {user:?} references absent channel {channel:?}")]
    UserChannelMissing { user: UserKey, channel: ChannelKey },
    #[error("user {user:?} references unknown source connection {source_connection:?}")]
    UnknownUserSource {
        user: UserKey,
        source_connection: ConnectionId,
    },
    #[error("source connection {source_connection:?} is projected by multiple users")]
    DuplicateUserSource { source_connection: ConnectionId },
    #[error("self connection {connection:?} is absent from its desired view")]
    SelfUserMissing { connection: ConnectionId },
    #[error("listener references absent user {user:?}")]
    ListenerUserMissing { user: UserKey },
    #[error("listener references absent channel {channel:?}")]
    ListenerChannelMissing { channel: ChannelKey },
    #[error("channel {channel:?} links to absent channel {linked:?}")]
    LinkedChannelMissing {
        channel: ChannelKey,
        linked: ChannelKey,
    },
    #[error("permissions reference absent channel {channel:?}")]
    PermissionChannelMissing { channel: ChannelKey },
}

/// Audio-route failures detected against the complete rendered connection set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum AudioRouteValidationError {
    #[error("route {route:?} references an unknown sender")]
    UnknownSender { route: DesiredAudioRoute },
    #[error("route {route:?} references an unknown receiver")]
    UnknownReceiver { route: DesiredAudioRoute },
    #[error("route {route:?} is declared by connection {owner:?}, not its receiver")]
    WrongOwner {
        owner: ConnectionId,
        route: DesiredAudioRoute,
    },
    #[error("receiver view does not project sender {sender:?} for route {route:?}")]
    SenderAbsentFromReceiverView {
        sender: ConnectionId,
        route: DesiredAudioRoute,
    },
}

/// Interaction-registry failures detected independently from view structure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum InteractionRegistryValidationError {
    #[error("action map key {map_key:?} differs from declared key {declared_key:?}")]
    ActionKeyMismatch {
        map_key: ActionKey,
        declared_key: ActionKey,
    },
    #[error("presented action {action:?} has no registry entry")]
    MissingRegistryEntry { action: ActionKey },
    #[error("registry action {action:?} is not presented in the desired view")]
    UnpresentedRegistryEntry { action: ActionKey },
}

/// Identifies the connection and validation pass that rejected a generation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum FlavorOutputValidationError {
    #[error("invalid desired view for connection {connection:?}: {source}")]
    DesiredView {
        connection: ConnectionId,
        #[source]
        source: DesiredViewValidationError,
    },
    #[error("invalid audio routes for connection {connection:?}: {source}")]
    AudioRoutes {
        connection: ConnectionId,
        #[source]
        source: AudioRouteValidationError,
    },
    #[error("invalid interaction registry for connection {connection:?}: {source}")]
    InteractionRegistry {
        connection: ConnectionId,
        #[source]
        source: InteractionRegistryValidationError,
    },
}

/// Validate every output of one rendered snapshot without applying any effect.
///
/// Passes run separately and in deterministic connection order: desired views,
/// audio routes, then interaction registries. Any failure consumes and drops
/// the whole candidate, so no partial validated generation can escape.
pub fn validate_rendered_snapshot<S>(
    rendered: RenderedSnapshot<S>,
) -> Result<ValidatedSnapshot<S>, FlavorOutputValidationError> {
    let known_connections: BTreeSet<ConnectionId> = rendered.outputs.keys().copied().collect();

    for (connection, output) in &rendered.outputs {
        validate_desired_view(*connection, output.client_view(), &known_connections).map_err(
            |source| FlavorOutputValidationError::DesiredView {
                connection: *connection,
                source,
            },
        )?;
    }

    for (connection, output) in &rendered.outputs {
        validate_audio_routes(*connection, output, &rendered.outputs).map_err(|source| {
            FlavorOutputValidationError::AudioRoutes {
                connection: *connection,
                source,
            }
        })?;
    }

    for (connection, output) in &rendered.outputs {
        validate_interactions(output).map_err(|source| {
            FlavorOutputValidationError::InteractionRegistry {
                connection: *connection,
                source,
            }
        })?;
    }

    Ok(ValidatedSnapshot { rendered })
}

fn validate_desired_view(
    connection: ConnectionId,
    view: &DesiredClientView,
    known_connections: &BTreeSet<ConnectionId>,
) -> Result<(), DesiredViewValidationError> {
    for (map_key, channel) in &view.channels {
        if map_key != &channel.key {
            return Err(DesiredViewValidationError::ChannelKeyMismatch {
                map_key: map_key.clone(),
                declared_key: channel.key.clone(),
            });
        }
    }

    let root = view.channels.get(&view.root_channel).ok_or_else(|| {
        DesiredViewValidationError::RootChannelMissing {
            root: view.root_channel.clone(),
        }
    })?;
    if root.parent != view.root_channel {
        return Err(DesiredViewValidationError::RootParentMismatch {
            root: view.root_channel.clone(),
            parent: root.parent.clone(),
        });
    }

    for channel in view.channels.values() {
        if channel.key != view.root_channel && !view.channels.contains_key(&channel.parent) {
            return Err(DesiredViewValidationError::MissingParent {
                channel: channel.key.clone(),
                parent: channel.parent.clone(),
            });
        }
    }

    for start in view.channels.keys() {
        let mut seen = BTreeSet::new();
        let mut current = start;
        loop {
            if !seen.insert(current.clone()) {
                return Err(DesiredViewValidationError::ParentCycle {
                    channel: start.clone(),
                    repeated: current.clone(),
                });
            }
            if current == &view.root_channel {
                break;
            }
            match view.channels.get(current) {
                Some(channel) => current = &channel.parent,
                None => break,
            }
        }
    }

    let mut projected_sources = BTreeSet::new();
    for (map_key, user) in &view.users {
        if map_key != &user.key {
            return Err(DesiredViewValidationError::UserKeyMismatch {
                map_key: map_key.clone(),
                declared_key: user.key.clone(),
            });
        }
        if !view.channels.contains_key(&user.channel) {
            return Err(DesiredViewValidationError::UserChannelMissing {
                user: user.key.clone(),
                channel: user.channel.clone(),
            });
        }
        if let Some(source) = user.source_connection {
            if !known_connections.contains(&source) {
                return Err(DesiredViewValidationError::UnknownUserSource {
                    user: user.key.clone(),
                    source_connection: source,
                });
            }
            if !projected_sources.insert(source) {
                return Err(DesiredViewValidationError::DuplicateUserSource {
                    source_connection: source,
                });
            }
        }
    }
    if !projected_sources.contains(&connection) {
        return Err(DesiredViewValidationError::SelfUserMissing { connection });
    }

    for listener in &view.listeners {
        if !view.users.contains_key(&listener.user) {
            return Err(DesiredViewValidationError::ListenerUserMissing {
                user: listener.user.clone(),
            });
        }
        if !view.channels.contains_key(&listener.channel) {
            return Err(DesiredViewValidationError::ListenerChannelMissing {
                channel: listener.channel.clone(),
            });
        }
    }
    for channel in view.channels.values() {
        for linked in &channel.links {
            if !view.channels.contains_key(linked) {
                return Err(DesiredViewValidationError::LinkedChannelMissing {
                    channel: channel.key.clone(),
                    linked: linked.clone(),
                });
            }
        }
    }
    for channel in view.permissions.keys() {
        if !view.channels.contains_key(channel) {
            return Err(DesiredViewValidationError::PermissionChannelMissing {
                channel: channel.clone(),
            });
        }
    }

    Ok(())
}

fn validate_audio_routes(
    owner: ConnectionId,
    output: &RenderOutput,
    outputs: &BTreeMap<ConnectionId, RenderOutput>,
) -> Result<(), AudioRouteValidationError> {
    for route in output.audio_routes() {
        if !outputs.contains_key(&route.sender) {
            return Err(AudioRouteValidationError::UnknownSender { route: *route });
        }
        let receiver_output = outputs
            .get(&route.receiver)
            .ok_or(AudioRouteValidationError::UnknownReceiver { route: *route })?;
        if route.receiver != owner {
            return Err(AudioRouteValidationError::WrongOwner {
                owner,
                route: *route,
            });
        }
        let receiver_knows_sender = receiver_output
            .client_view()
            .users
            .values()
            .any(|user| user.source_connection == Some(route.sender));
        if !receiver_knows_sender {
            return Err(AudioRouteValidationError::SenderAbsentFromReceiverView {
                sender: route.sender,
                route: *route,
            });
        }
    }
    Ok(())
}

fn validate_interactions(output: &RenderOutput) -> Result<(), InteractionRegistryValidationError> {
    for (map_key, action) in &output.client_view().context_actions {
        if map_key != &action.key {
            return Err(InteractionRegistryValidationError::ActionKeyMismatch {
                map_key: map_key.clone(),
                declared_key: action.key.clone(),
            });
        }
        if !output.interactions().actions().contains(map_key) {
            return Err(InteractionRegistryValidationError::MissingRegistryEntry {
                action: map_key.clone(),
            });
        }
    }
    for action in output.interactions().actions() {
        if !output.client_view().context_actions.contains_key(action) {
            return Err(
                InteractionRegistryValidationError::UnpresentedRegistryEntry {
                    action: action.clone(),
                },
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod flavor_output_validation {
    use voxloom_flavor::{
        ActionTarget, ContextActionView, DesiredUser, InteractionRegistry, SemanticKey,
    };

    use super::*;

    fn user_key(connection: ConnectionId) -> UserKey {
        UserKey(SemanticKey::Dynamic(connection.get()))
    }

    fn desired_user(connection: ConnectionId, channel: ChannelKey) -> DesiredUser {
        DesiredUser {
            key: user_key(connection),
            source_connection: Some(connection),
            name: format!("connection-{}", connection.get()),
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

    fn desired_view(
        connection: ConnectionId,
        visible_sources: impl IntoIterator<Item = ConnectionId>,
    ) -> DesiredClientView {
        let mut view = DesiredClientView::empty();
        for source in visible_sources {
            let user = desired_user(source, view.root_channel.clone());
            view.users.insert(user.key.clone(), user);
        }
        assert!(view.users.contains_key(&user_key(connection)));
        view
    }

    fn output(view: DesiredClientView, routes: BTreeSet<DesiredAudioRoute>) -> RenderOutput {
        RenderOutput::new(view, routes, InteractionRegistry::default())
    }

    fn rendered(outputs: BTreeMap<ConnectionId, RenderOutput>) -> RenderedSnapshot<&'static str> {
        RenderedSnapshot {
            snapshot: Arc::new("snapshot"),
            flavor_revision: FlavorRevision::new(7),
            outputs,
        }
    }

    fn valid_outputs() -> BTreeMap<ConnectionId, RenderOutput> {
        let sender = ConnectionId::new(1);
        let receiver = ConnectionId::new(2);
        BTreeMap::from([
            (
                sender,
                output(desired_view(sender, [sender]), BTreeSet::new()),
            ),
            (
                receiver,
                output(
                    desired_view(receiver, [sender, receiver]),
                    BTreeSet::from([DesiredAudioRoute { sender, receiver }]),
                ),
            ),
        ])
    }

    #[test]
    fn flavor_output_validation_accepts_one_complete_generation() {
        let candidate = rendered(valid_outputs());
        let snapshot = Arc::clone(candidate.snapshot());
        let result = validate_rendered_snapshot(candidate);

        match result {
            Ok(validated) => {
                assert!(Arc::ptr_eq(validated.snapshot(), &snapshot));
                assert_eq!(validated.flavor_revision(), FlavorRevision::new(7));
                assert_eq!(validated.outputs().len(), 2);
            }
            Err(error) => panic!("valid flavor output was rejected: {error}"),
        }
    }

    #[test]
    fn flavor_output_validation_rejects_the_view_before_other_passes() {
        let connection = ConnectionId::new(1);
        let mut view = desired_view(connection, [connection]);
        view.channels.remove(&view.root_channel);
        let unknown = ConnectionId::new(99);
        let action = ActionKey(SemanticKey::Static("inspect".to_owned()));
        let registry = InteractionRegistry::new(BTreeSet::from([action]));
        let candidate = rendered(BTreeMap::from([(
            connection,
            RenderOutput::new(
                view,
                BTreeSet::from([DesiredAudioRoute {
                    sender: connection,
                    receiver: unknown,
                }]),
                registry,
            ),
        )]));
        let result = validate_rendered_snapshot(candidate);

        assert_eq!(
            result.err(),
            Some(FlavorOutputValidationError::DesiredView {
                connection,
                source: DesiredViewValidationError::RootChannelMissing {
                    root: ChannelKey(SemanticKey::Static("root".to_owned())),
                },
            })
        );
    }

    #[test]
    fn flavor_output_validation_rejects_an_unknown_route_sender() {
        let connection = ConnectionId::new(1);
        let route = DesiredAudioRoute {
            sender: ConnectionId::new(99),
            receiver: connection,
        };
        let candidate = rendered(BTreeMap::from([(
            connection,
            output(
                desired_view(connection, [connection]),
                BTreeSet::from([route]),
            ),
        )]));
        let result = validate_rendered_snapshot(candidate);

        assert_eq!(
            result.err(),
            Some(FlavorOutputValidationError::AudioRoutes {
                connection,
                source: AudioRouteValidationError::UnknownSender { route },
            })
        );
    }

    #[test]
    fn flavor_output_validation_requires_the_receiver_to_see_the_sender() {
        let sender = ConnectionId::new(1);
        let receiver = ConnectionId::new(2);
        let route = DesiredAudioRoute { sender, receiver };
        let candidate = rendered(BTreeMap::from([
            (
                sender,
                output(desired_view(sender, [sender]), BTreeSet::new()),
            ),
            (
                receiver,
                output(desired_view(receiver, [receiver]), BTreeSet::from([route])),
            ),
        ]));
        let result = validate_rendered_snapshot(candidate);

        assert_eq!(
            result.err(),
            Some(FlavorOutputValidationError::AudioRoutes {
                connection: receiver,
                source: AudioRouteValidationError::SenderAbsentFromReceiverView { sender, route },
            })
        );
    }

    #[test]
    fn flavor_output_validation_requires_the_receiver_to_own_its_routes() {
        let sender = ConnectionId::new(1);
        let receiver = ConnectionId::new(2);
        let route = DesiredAudioRoute { sender, receiver };
        let candidate = rendered(BTreeMap::from([
            (
                sender,
                output(desired_view(sender, [sender]), BTreeSet::from([route])),
            ),
            (
                receiver,
                output(desired_view(receiver, [sender, receiver]), BTreeSet::new()),
            ),
        ]));
        let result = validate_rendered_snapshot(candidate);

        assert_eq!(
            result.err(),
            Some(FlavorOutputValidationError::AudioRoutes {
                connection: sender,
                source: AudioRouteValidationError::WrongOwner {
                    owner: sender,
                    route,
                },
            })
        );
    }

    #[test]
    fn flavor_output_validation_rejects_an_unregistered_presented_action() {
        let connection = ConnectionId::new(1);
        let mut view = desired_view(connection, [connection]);
        let action = ActionKey(SemanticKey::Static("inspect".to_owned()));
        view.context_actions.insert(
            action.clone(),
            ContextActionView {
                key: action.clone(),
                target: ActionTarget::User,
                label: "Inspect".to_owned(),
            },
        );
        let candidate = rendered(BTreeMap::from([(
            connection,
            output(view, BTreeSet::new()),
        )]));
        let result = validate_rendered_snapshot(candidate);

        assert_eq!(
            result.err(),
            Some(FlavorOutputValidationError::InteractionRegistry {
                connection,
                source: InteractionRegistryValidationError::MissingRegistryEntry { action },
            })
        );
    }
}
