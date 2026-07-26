//! Atomic publication of one validated flavor generation.
//!
//! A generation is published in three stages, in this order:
//!
//! 1. the routing snapshot is republished with every revoked route already
//!    removed, so a forbidden flow stops before the view that justified it is
//!    taken away (spec 12.6, invariant 18);
//! 2. every connection's view transaction is delivered;
//! 3. the routing snapshot is republished with the newly authorized routes,
//!    which is only reachable through [`PublicationCoordinator::commit`] and
//!    therefore only after the receiver's view has been committed (invariant
//!    19).
//!
//! Stage 1 happens inside [`PublicationCoordinator::publish`] because revoking
//! is unconditionally safe: a caller that abandons the pending publication
//! leaves the connections at their previous committed views with less audio
//! than they were entitled to, never more. Stages 2 and 3 are all-or-nothing
//! for the whole generation: a delivery that cannot be admitted downstream
//! means the [`PublicationCommit`] is dropped and nothing is committed, and the
//! next publication is planned from the unchanged committed views.
//!
//! REF: docs/voxloom-roadmap-agents-v0_1.md P7 T4
//! REF: docs/voxloom-specification-technique-v0.1.md 12.6, 23.3

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use thiserror::Error;
use voxloom_audio::{
    AudioRoutingSnapshot, DirectedRoute, Participant, RoutingDomainId, SessionId as AudioSessionId,
    compile_authorized,
};
use voxloom_flavor::{
    ChannelKey, ConnectionId, DesiredAudioRoute, DesiredClientView, FlavorRevision, UserKey,
};
use voxloom_protocol::ControlMessage;
use voxloom_reconcile::{AudioRoute, ChannelIdKind, IdError, ViewIdMapping};
use voxloom_render::{ChannelId, ClientView, ListenerRelation, SessionId, ViewChannel, ViewUser};
use voxloom_session::{CommitToken, ConnectionView, EmittedStep, TransitionError};

use crate::ValidatedSnapshot;

/// Why a generation could not be planned, published or committed.
///
/// Every variant names the connection it belongs to: a publication spans all of
/// them, and "publication refused" without a connection is unactionable.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum PublicationError {
    #[error("connection {connection:?} is already registered")]
    DuplicateConnection { connection: ConnectionId },
    #[error("session {session:?} is already bound to another connection")]
    DuplicateSession { session: SessionId },
    #[error("connection {connection:?} is not registered")]
    UnknownConnection { connection: ConnectionId },
    #[error("registered connection {connection:?} has no output in this generation")]
    MissingConnectionOutput { connection: ConnectionId },
    #[error(
        "connection {connection:?} projects user {user:?} without a source connection; P7 \
         assigns no session to a synthetic user (spec 9.4)"
    )]
    SyntheticUserUnsupported {
        connection: ConnectionId,
        user: UserKey,
    },
    #[error("connection {connection:?} cannot resolve channel key {channel:?}")]
    UnresolvedChannel {
        connection: ConnectionId,
        channel: ChannelKey,
    },
    #[error("connection {connection:?} cannot resolve user key {user:?}")]
    UnresolvedUser {
        connection: ConnectionId,
        user: UserKey,
    },
    #[error("connection {connection:?} exhausted its channel id space: {source}")]
    ChannelIdExhausted {
        connection: ConnectionId,
        #[source]
        source: IdError,
    },
    #[error("connection {connection:?} refused the resolved view: {source}")]
    RejectedView {
        connection: ConnectionId,
        #[source]
        source: TransitionError,
    },
    #[error("the monotonic generation counter is exhausted")]
    GenerationExhausted,
    #[error(
        "publication was planned at epoch {expected} but the coordinator is at {found}; refusing \
         to commit a generation planned against superseded state"
    )]
    StalePublication { expected: u64, found: u64 },
}

/// One registered voice connection and the view lifecycle it owns.
#[derive(Debug)]
struct Connection {
    session: SessionId,
    view: ConnectionView,
}

/// The control-plane serializer of voice generations (spec 23.1).
///
/// It owns nothing of the business state: connections, their committed views
/// and the published routing snapshot are the whole of it.
#[derive(Debug)]
pub struct PublicationCoordinator {
    generation: u64,
    /// Monotonic stamp of the published routing snapshot. It advances twice per
    /// committed generation (revocation, then grant), so it is tracked apart
    /// from the Voxloom generation rather than derived from it.
    audio_generation: u64,
    /// Bumped by anything that invalidates a planned generation. A
    /// [`PublicationCommit`] carries the epoch it was planned at and is refused
    /// once the coordinator has moved on.
    epoch: u64,
    audio: Arc<AudioRoutingSnapshot>,
    connections: BTreeMap<ConnectionId, Connection>,
}

impl Default for PublicationCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

/// A planned generation: the frames to deliver and the proof needed to commit.
#[derive(Debug)]
#[must_use = "a pending publication that is neither split nor dropped delivers nothing"]
pub struct PendingPublication {
    deliveries: BTreeMap<ConnectionId, Vec<ControlMessage>>,
    commit: PublicationCommit,
}

/// Proof that a specific generation was planned, and the state it commits to.
#[derive(Debug)]
#[must_use = "dropping the commit abandons the generation; revoked audio stays revoked"]
pub struct PublicationCommit {
    epoch: u64,
    generation: u64,
    flavor_revision: FlavorRevision,
    tokens: Vec<(ConnectionId, CommitToken)>,
}

/// A committed generation (spec 23.3 `struct PublishedGeneration`).
#[derive(Debug, Clone)]
pub struct PublishedGeneration {
    generation: u64,
    flavor_revision: FlavorRevision,
    view_revisions: BTreeMap<ConnectionId, u64>,
    audio: Arc<AudioRoutingSnapshot>,
}

impl PublishedGeneration {
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub const fn flavor_revision(&self) -> FlavorRevision {
        self.flavor_revision
    }

    /// The committed view revision each connection now holds.
    #[must_use]
    pub const fn view_revisions(&self) -> &BTreeMap<ConnectionId, u64> {
        &self.view_revisions
    }

    /// The routing snapshot including the routes this generation granted.
    #[must_use]
    pub const fn audio(&self) -> &Arc<AudioRoutingSnapshot> {
        &self.audio
    }
}

impl PendingPublication {
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.commit.generation
    }

    #[must_use]
    pub const fn flavor_revision(&self) -> FlavorRevision {
        self.commit.flavor_revision
    }

    /// The control frames each connection must be sent, in order. Connections
    /// whose view did not change are absent rather than present and empty.
    #[must_use]
    pub const fn deliveries(&self) -> &BTreeMap<ConnectionId, Vec<ControlMessage>> {
        &self.deliveries
    }

    /// Split into the frames to deliver and the commit that follows them.
    pub fn split(
        self,
    ) -> (
        BTreeMap<ConnectionId, Vec<ControlMessage>>,
        PublicationCommit,
    ) {
        (self.deliveries, self.commit)
    }
}

impl PublicationCommit {
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

impl PublicationCoordinator {
    /// A coordinator with no connection and an empty routing snapshot.
    #[must_use]
    pub fn new() -> Self {
        Self {
            generation: 0,
            audio_generation: 0,
            epoch: 0,
            audio: Arc::new(compile_authorized(&[], &[], 0)),
            connections: BTreeMap::new(),
        }
    }

    /// The last committed Voxloom generation. Monotonic, only moved by
    /// [`PublicationCoordinator::commit`].
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// The routing snapshot the audio plane must currently consult (spec 23.2).
    #[must_use]
    pub const fn audio(&self) -> &Arc<AudioRoutingSnapshot> {
        &self.audio
    }

    /// What `connection` is assumed to be showing, if it is registered.
    #[must_use]
    pub fn committed_view(&self, connection: ConnectionId) -> Option<&ClientView> {
        self.connections
            .get(&connection)
            .map(|entry| entry.view.committed())
    }

    /// One connection's view lifecycle, for the inbound direction: resolving a
    /// client-supplied id is only meaningful against the view that connection
    /// actually holds.
    pub(crate) fn connection_view(&self, connection: ConnectionId) -> Option<&ConnectionView> {
        self.connections.get(&connection).map(|entry| &entry.view)
    }

    /// The audio routes that go with `connection`'s committed view.
    #[must_use]
    pub fn committed_routes(&self, connection: ConnectionId) -> Option<&BTreeSet<AudioRoute>> {
        self.connections
            .get(&connection)
            .map(|entry| entry.view.committed_routes())
    }

    /// Bind a voice connection to the session id the runtime allocated for it.
    ///
    /// Two connections sharing a session would make the routing table
    /// ambiguous, so the second one is refused rather than merged.
    pub fn register(
        &mut self,
        connection: ConnectionId,
        session: SessionId,
    ) -> Result<(), PublicationError> {
        if self.connections.contains_key(&connection) {
            return Err(PublicationError::DuplicateConnection { connection });
        }
        if self
            .connections
            .values()
            .any(|entry| entry.session == session)
        {
            return Err(PublicationError::DuplicateSession { session });
        }
        self.connections.insert(
            connection,
            Connection {
                session,
                view: ConnectionView::new(session),
            },
        );
        self.invalidate_pending();
        Ok(())
    }

    /// Drop a connection and republish the routing snapshot without it.
    ///
    /// Routes held by surviving connections that named the departed one are
    /// dropped by the compiler, which only keeps routes between known
    /// participants. Returns the session that was freed, if any.
    pub fn unregister(&mut self, connection: ConnectionId) -> Option<SessionId> {
        let entry = self.connections.remove(&connection)?;
        self.invalidate_pending();
        self.republish_audio(&self.committed_route_union());
        Some(entry.session)
    }

    /// Plan one generation and publish its revocation stage.
    ///
    /// Every registered connection must appear in the validated outputs and
    /// vice versa: a generation that skips a connection would leave it showing
    /// a view derived from another snapshot revision.
    ///
    /// On success the routing snapshot has already lost every revoked route.
    /// On error nothing is published and every committed view is untouched;
    /// channel ids allocated while resolving are kept, which only advances a
    /// monotonic cursor (invariant 12).
    pub fn publish<S>(
        &mut self,
        validated: &ValidatedSnapshot<S>,
    ) -> Result<PendingPublication, PublicationError> {
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(PublicationError::GenerationExhausted)?;

        for connection in validated.outputs().keys() {
            if !self.connections.contains_key(connection) {
                return Err(PublicationError::UnknownConnection {
                    connection: *connection,
                });
            }
        }
        for connection in self.connections.keys() {
            if !validated.outputs().contains_key(connection) {
                return Err(PublicationError::MissingConnectionOutput {
                    connection: *connection,
                });
            }
        }

        let sessions: BTreeMap<ConnectionId, SessionId> = self
            .connections
            .iter()
            .map(|(connection, entry)| (*connection, entry.session))
            .collect();

        let mut deliveries = BTreeMap::new();
        let mut tokens = Vec::new();
        let mut surviving: BTreeSet<AudioRoute> = BTreeSet::new();

        for (connection, output) in validated.outputs() {
            let entry = self.connections.get_mut(connection).ok_or(
                PublicationError::UnknownConnection {
                    connection: *connection,
                },
            )?;
            let desired_view = resolve_view(
                *connection,
                output.client_view(),
                &sessions,
                entry.view.ids_mut(),
            )?;
            let desired_routes = resolve_routes(output.audio_routes(), &sessions)?;

            // Stage 1 keeps exactly the routes that both the committed state and
            // this generation authorize. Everything else is a revocation and
            // must stop before its view disappears.
            surviving.extend(
                entry
                    .view
                    .committed_routes()
                    .intersection(&desired_routes)
                    .copied(),
            );

            let prepared =
                entry
                    .view
                    .prepare(&desired_view, &desired_routes)
                    .map_err(|source| PublicationError::RejectedView {
                        connection: *connection,
                        source,
                    })?;
            let Some(prepared) = prepared else {
                continue;
            };
            let (steps, token) = prepared.split();

            // Route toggles are dropped here rather than handed to the runtime:
            // this coordinator owns the routing snapshot, and applying a toggle
            // twice from two owners is how a revocation gets undone.
            let messages = steps
                .into_iter()
                .filter_map(|step| match step {
                    EmittedStep::Message(message) => Some(message),
                    EmittedStep::RouteChange { .. } => None,
                })
                .collect();
            deliveries.insert(*connection, messages);
            tokens.push((*connection, token));
        }

        self.republish_audio(&surviving);
        self.invalidate_pending();

        Ok(PendingPublication {
            deliveries,
            commit: PublicationCommit {
                epoch: self.epoch,
                generation,
                flavor_revision: validated.flavor_revision(),
                tokens,
            },
        })
    }

    /// Commit a delivered generation and publish its grant stage.
    ///
    /// Call only once every frame of every delivery has been accepted
    /// downstream. The coordinator refuses a commit planned before any later
    /// registration, unregistration or publication.
    pub fn commit(
        &mut self,
        commit: PublicationCommit,
    ) -> Result<PublishedGeneration, PublicationError> {
        if commit.epoch != self.epoch {
            return Err(PublicationError::StalePublication {
                expected: commit.epoch,
                found: self.epoch,
            });
        }

        for (connection, token) in commit.tokens {
            let entry = self
                .connections
                .get_mut(&connection)
                .ok_or(PublicationError::UnknownConnection { connection })?;
            entry
                .view
                .commit(token)
                .map_err(|source| PublicationError::RejectedView { connection, source })?;
        }

        self.generation = commit.generation;
        self.republish_audio(&self.committed_route_union());
        self.invalidate_pending();

        Ok(PublishedGeneration {
            generation: self.generation,
            flavor_revision: commit.flavor_revision,
            view_revisions: self
                .connections
                .iter()
                .map(|(connection, entry)| (*connection, entry.view.revision()))
                .collect(),
            audio: Arc::clone(&self.audio),
        })
    }

    /// Every route currently justified by a committed view.
    fn committed_route_union(&self) -> BTreeSet<AudioRoute> {
        self.connections
            .values()
            .flat_map(|entry| entry.view.committed_routes().iter().copied())
            .collect()
    }

    /// Compile and swap in a routing snapshot (ADR-005 cold path).
    ///
    /// Every connection shares one routing domain: in P7 a flow is authorized
    /// by the flavor's per-receiver route declaration, not by a partition the
    /// runtime invents. Domains stay in the snapshot for the audio plane's own
    /// policy checks.
    fn republish_audio(&mut self, routes: &BTreeSet<AudioRoute>) {
        let participants: Vec<Participant> = self
            .connections
            .values()
            .map(|entry| {
                Participant::new(
                    AudioSessionId::new(entry.session.0),
                    RoutingDomainId::DEFAULT,
                )
            })
            .collect();
        let authorized: Vec<DirectedRoute> = routes
            .iter()
            .map(|route| {
                DirectedRoute::new(
                    AudioSessionId::new(route.sender.0),
                    AudioSessionId::new(route.receiver.0),
                )
            })
            .collect();
        // Saturating is the fail-closed choice for a stamp that only has to be
        // comparable: a stuck generation makes a reader keep an older table, it
        // never broadens delivery.
        self.audio_generation = self.audio_generation.saturating_add(1);
        self.audio = Arc::new(compile_authorized(
            &participants,
            &authorized,
            self.audio_generation,
        ));
    }

    /// Retire every outstanding [`PublicationCommit`].
    fn invalidate_pending(&mut self) {
        // Saturating for the same reason as the audio stamp: a stuck epoch
        // refuses commits, which keeps committed views where they are.
        self.epoch = self.epoch.saturating_add(1);
    }
}

/// Turn one connection's semantic view into the numeric view it will hold.
///
/// The view has already passed [`crate::validate_rendered_snapshot`], so every
/// key it references exists; the lookups below still fail closed rather than
/// index, because a resolution gap must not become a panic in a live server.
fn resolve_view(
    connection: ConnectionId,
    desired: &DesiredClientView,
    sessions: &BTreeMap<ConnectionId, SessionId>,
    ids: &mut ViewIdMapping,
) -> Result<ClientView, PublicationError> {
    let mut channel_ids: BTreeMap<ChannelKey, ChannelId> =
        BTreeMap::from([(desired.root_channel.clone(), ChannelId::ROOT)]);
    for channel in desired.channels.values() {
        if channel.key == desired.root_channel {
            continue;
        }
        // A temporary channel draws from the reserved ephemeral range so the
        // client's per-id local preferences stay attached to durable channels
        // (spec 9.3).
        let kind = if channel.temporary {
            ChannelIdKind::Ephemeral
        } else {
            ChannelIdKind::Stable
        };
        let id = ids
            .resolve(channel.key.clone(), kind)
            .map_err(|source| PublicationError::ChannelIdExhausted { connection, source })?;
        channel_ids.insert(channel.key.clone(), id);
    }

    let channel_id = |key: &ChannelKey| -> Result<ChannelId, PublicationError> {
        channel_ids
            .get(key)
            .copied()
            .ok_or_else(|| PublicationError::UnresolvedChannel {
                connection,
                channel: key.clone(),
            })
    };

    let mut channels = BTreeMap::new();
    for channel in desired.channels.values() {
        let id = channel_id(&channel.key)?;
        let parent = channel_id(&channel.parent)?;
        let mut links = BTreeSet::new();
        for linked in &channel.links {
            links.insert(channel_id(linked)?);
        }
        channels.insert(
            id,
            ViewChannel {
                key: channel.key.clone(),
                id,
                parent,
                name: channel.name.clone(),
                description: channel.description.clone(),
                position: channel.sort_order,
                temporary: channel.temporary,
                max_users: channel.max_users,
                enter_restricted: channel.enter_restricted,
                can_enter: channel.can_enter,
                links,
            },
        );
    }

    let mut users = BTreeMap::new();
    let mut user_sessions: BTreeMap<UserKey, SessionId> = BTreeMap::new();
    for user in desired.users.values() {
        // Spec 9.4 allows a synthetic user, but no P7 task specifies how its
        // session is allocated or how audio references it. Refusing loudly is
        // the fail-closed answer; inventing an allocation policy is not.
        let source =
            user.source_connection
                .ok_or_else(|| PublicationError::SyntheticUserUnsupported {
                    connection,
                    user: user.key.clone(),
                })?;
        let session = sessions
            .get(&source)
            .copied()
            .ok_or(PublicationError::UnknownConnection { connection: source })?;
        let channel = channel_id(&user.channel)?;
        user_sessions.insert(user.key.clone(), session);
        users.insert(
            session,
            ViewUser {
                key: user.key.clone(),
                session,
                name: user.name.clone(),
                channel,
                user_id: user.user_id,
                certificate_hash: user.certificate_hash.clone(),
                mute: user.mute,
                deaf: user.deaf,
                suppress: user.suppress,
                self_mute: user.self_mute,
                self_deaf: user.self_deaf,
                priority_speaker: user.priority_speaker,
                recording: user.recording,
                comment: user.comment.clone(),
                texture: user.texture.clone(),
            },
        );
    }

    let mut listeners = BTreeSet::new();
    for listener in &desired.listeners {
        let session = user_sessions.get(&listener.user).copied().ok_or_else(|| {
            PublicationError::UnresolvedUser {
                connection,
                user: listener.user.clone(),
            }
        })?;
        listeners.insert(ListenerRelation {
            user: session,
            channel: channel_id(&listener.channel)?,
        });
    }

    let mut permissions = BTreeMap::new();
    for (key, bits) in &desired.permissions {
        permissions.insert(channel_id(key)?, *bits);
    }

    Ok(ClientView {
        root_channel: ChannelId::ROOT,
        channels,
        users,
        listeners,
        permissions,
        context_actions: desired.context_actions.clone(),
        server_presentation: desired.server_presentation.clone(),
    })
}

/// Turn one connection's declared routes into session-addressed routes.
///
/// Ownership by the receiver is a validation property (spec 24.1) and is not
/// re-derived here; only the identities are translated.
fn resolve_routes(
    desired: &BTreeSet<DesiredAudioRoute>,
    sessions: &BTreeMap<ConnectionId, SessionId>,
) -> Result<BTreeSet<AudioRoute>, PublicationError> {
    let mut routes = BTreeSet::new();
    for route in desired {
        let sender =
            sessions
                .get(&route.sender)
                .copied()
                .ok_or(PublicationError::UnknownConnection {
                    connection: route.sender,
                })?;
        let receiver =
            sessions
                .get(&route.receiver)
                .copied()
                .ok_or(PublicationError::UnknownConnection {
                    connection: route.receiver,
                })?;
        routes.insert(AudioRoute { sender, receiver });
    }
    Ok(routes)
}

#[cfg(test)]
mod publication_order {
    use std::sync::Arc;

    use voxloom_flavor::{
        DesiredClientView, DesiredUser, FlavorError, InteractionRegistry, RenderOutput,
        SemanticKey, VoiceFlavor,
    };

    use super::*;
    use crate::{render_snapshot, validate_rendered_snapshot};

    /// Who each connection may see, itself included. Everything else about the
    /// rendered view follows from that single answer, which is what makes the
    /// ordering assertions below readable.
    #[derive(Debug)]
    struct Visibility {
        revision: FlavorRevision,
        visible: BTreeMap<ConnectionId, BTreeSet<ConnectionId>>,
    }

    #[derive(Debug)]
    struct VisibilityFlavor;

    impl VoiceFlavor for VisibilityFlavor {
        type Snapshot = Visibility;

        fn revision(&self, snapshot: &Self::Snapshot) -> FlavorRevision {
            snapshot.revision
        }

        fn render(
            &self,
            snapshot: &Self::Snapshot,
            connection: ConnectionId,
        ) -> Result<RenderOutput, FlavorError> {
            let visible = snapshot.visible.get(&connection).ok_or_else(|| {
                FlavorError::render_refused(connection, "connection is absent from the snapshot")
            })?;
            let mut view = DesiredClientView::empty();
            for source in visible {
                let key = UserKey(SemanticKey::Dynamic(source.get()));
                view.users.insert(
                    key.clone(),
                    DesiredUser {
                        key,
                        source_connection: Some(*source),
                        name: format!("connection-{}", source.get()),
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
            }
            let routes = visible
                .iter()
                .filter(|source| **source != connection)
                .map(|source| DesiredAudioRoute {
                    sender: *source,
                    receiver: connection,
                })
                .collect();
            Ok(RenderOutput::new(
                view,
                routes,
                InteractionRegistry::default(),
            ))
        }

        fn observe(&self, _event: &voxloom_flavor::VoiceEvent) {}
    }

    const FIRST: ConnectionId = ConnectionId::new(1);
    const SECOND: ConnectionId = ConnectionId::new(2);
    const FIRST_SESSION: SessionId = SessionId(11);
    const SECOND_SESSION: SessionId = SessionId(22);

    fn snapshot(revision: u64, mutual: bool) -> Arc<Visibility> {
        let visible = if mutual {
            BTreeMap::from([
                (FIRST, BTreeSet::from([FIRST, SECOND])),
                (SECOND, BTreeSet::from([FIRST, SECOND])),
            ])
        } else {
            BTreeMap::from([
                (FIRST, BTreeSet::from([FIRST])),
                (SECOND, BTreeSet::from([SECOND])),
            ])
        };
        Arc::new(Visibility {
            revision: FlavorRevision::new(revision),
            visible,
        })
    }

    fn coordinator() -> PublicationCoordinator {
        let mut coordinator = PublicationCoordinator::new();
        match coordinator.register(FIRST, FIRST_SESSION) {
            Ok(()) => {}
            Err(error) => panic!("first registration failed: {error}"),
        }
        match coordinator.register(SECOND, SECOND_SESSION) {
            Ok(()) => {}
            Err(error) => panic!("second registration failed: {error}"),
        }
        coordinator
    }

    fn plan(
        coordinator: &mut PublicationCoordinator,
        snapshot: Arc<Visibility>,
    ) -> PendingPublication {
        let rendered = match render_snapshot(&VisibilityFlavor, snapshot, [FIRST, SECOND]) {
            Ok(rendered) => rendered,
            Err(error) => panic!("render failed: {error}"),
        };
        let validated = match validate_rendered_snapshot(rendered) {
            Ok(validated) => validated,
            Err(error) => panic!("validation failed: {error}"),
        };
        match coordinator.publish(&validated) {
            Ok(pending) => pending,
            Err(error) => panic!("publication failed: {error}"),
        }
    }

    fn publish_and_commit(coordinator: &mut PublicationCoordinator, snapshot: Arc<Visibility>) {
        let (_deliveries, commit) = plan(coordinator, snapshot).split();
        match coordinator.commit(commit) {
            Ok(_published) => {}
            Err(error) => panic!("commit failed: {error}"),
        }
    }

    /// Whether the published routing table currently carries sender -> receiver.
    fn hears(coordinator: &PublicationCoordinator, sender: SessionId, receiver: SessionId) -> bool {
        coordinator
            .audio()
            .routes()
            .recipients_of(AudioSessionId::new(sender.0))
            .contains(&AudioSessionId::new(receiver.0))
    }

    /// Whether `connection`'s committed view still projects `session`.
    fn view_projects(
        coordinator: &PublicationCoordinator,
        connection: ConnectionId,
        session: SessionId,
    ) -> bool {
        match coordinator.committed_view(connection) {
            Some(view) => view.users.contains_key(&session),
            None => panic!("connection {connection:?} is not registered"),
        }
    }

    #[test]
    fn publication_order_revokes_audio_before_removing_the_view() {
        let mut coordinator = coordinator();
        publish_and_commit(&mut coordinator, snapshot(1, true));
        assert!(hears(&coordinator, FIRST_SESSION, SECOND_SESSION));
        assert!(hears(&coordinator, SECOND_SESSION, FIRST_SESSION));

        let pending = plan(&mut coordinator, snapshot(2, false));

        // Stage 1 has already run: the flow is dead while both clients still
        // hold the view that justified it (spec 12.6, invariant 18).
        assert!(!hears(&coordinator, FIRST_SESSION, SECOND_SESSION));
        assert!(!hears(&coordinator, SECOND_SESSION, FIRST_SESSION));
        assert!(view_projects(&coordinator, SECOND, FIRST_SESSION));
        assert!(view_projects(&coordinator, FIRST, SECOND_SESSION));

        let (deliveries, commit) = pending.split();
        assert_eq!(deliveries.len(), 2, "both views lose a user");
        match coordinator.commit(commit) {
            Ok(published) => assert_eq!(published.generation(), 2),
            Err(error) => panic!("commit failed: {error}"),
        }

        assert!(!view_projects(&coordinator, SECOND, FIRST_SESSION));
        assert!(!hears(&coordinator, FIRST_SESSION, SECOND_SESSION));
    }

    #[test]
    fn publication_order_grants_audio_only_after_the_receiver_commits() {
        let mut coordinator = coordinator();
        publish_and_commit(&mut coordinator, snapshot(1, false));
        assert!(!hears(&coordinator, FIRST_SESSION, SECOND_SESSION));

        let pending = plan(&mut coordinator, snapshot(2, true));

        // The frames exist but nobody has been told anything yet, so the flow
        // stays off (spec 12.6, invariant 19).
        assert_eq!(pending.deliveries().len(), 2);
        assert!(!hears(&coordinator, FIRST_SESSION, SECOND_SESSION));
        assert!(!hears(&coordinator, SECOND_SESSION, FIRST_SESSION));
        assert!(!view_projects(&coordinator, SECOND, FIRST_SESSION));

        let (_deliveries, commit) = pending.split();
        match coordinator.commit(commit) {
            Ok(published) => {
                assert_eq!(published.flavor_revision(), FlavorRevision::new(2));
                assert_eq!(
                    published.view_revisions().get(&SECOND).copied(),
                    Some(2),
                    "the second connection committed two generations"
                );
            }
            Err(error) => panic!("commit failed: {error}"),
        }

        assert!(view_projects(&coordinator, SECOND, FIRST_SESSION));
        assert!(hears(&coordinator, FIRST_SESSION, SECOND_SESSION));
        assert!(hears(&coordinator, SECOND_SESSION, FIRST_SESSION));
    }

    #[test]
    fn publication_order_advances_the_generation_only_on_commit() {
        let mut coordinator = coordinator();
        assert_eq!(coordinator.generation(), 0);

        let pending = plan(&mut coordinator, snapshot(1, true));
        assert_eq!(pending.generation(), 1);
        assert_eq!(coordinator.generation(), 0, "planning commits nothing");

        let (_deliveries, commit) = pending.split();
        match coordinator.commit(commit) {
            Ok(published) => assert_eq!(published.generation(), 1),
            Err(error) => panic!("commit failed: {error}"),
        }
        assert_eq!(coordinator.generation(), 1);

        publish_and_commit(&mut coordinator, snapshot(2, false));
        assert_eq!(coordinator.generation(), 2);
    }

    #[test]
    fn publication_order_refuses_a_generation_planned_against_superseded_state() {
        let mut coordinator = coordinator();
        publish_and_commit(&mut coordinator, snapshot(1, true));

        let overtaken = plan(&mut coordinator, snapshot(2, false));
        let newest = plan(&mut coordinator, snapshot(3, false));

        let (_deliveries, stale) = overtaken.split();
        match coordinator.commit(stale) {
            Err(PublicationError::StalePublication { .. }) => {}
            Err(error) => panic!("unexpected error: {error}"),
            Ok(_published) => panic!("a superseded generation must not commit"),
        }
        assert_eq!(coordinator.generation(), 1, "nothing was committed");
        assert!(view_projects(&coordinator, SECOND, FIRST_SESSION));

        let (_deliveries, commit) = newest.split();
        match coordinator.commit(commit) {
            Ok(published) => assert_eq!(published.generation(), 2),
            Err(error) => panic!("commit failed: {error}"),
        }
    }

    #[test]
    fn publication_order_keeps_committed_views_when_a_generation_is_abandoned() {
        let mut coordinator = coordinator();
        publish_and_commit(&mut coordinator, snapshot(1, true));

        drop(plan(&mut coordinator, snapshot(2, false)));

        // The revocation stands and the views did not move: the connections
        // hold strictly less audio than their views allow, never more.
        assert!(!hears(&coordinator, FIRST_SESSION, SECOND_SESSION));
        assert!(view_projects(&coordinator, SECOND, FIRST_SESSION));
        assert_eq!(coordinator.generation(), 1);

        publish_and_commit(&mut coordinator, snapshot(3, true));
        assert!(hears(&coordinator, FIRST_SESSION, SECOND_SESSION));
        assert_eq!(coordinator.generation(), 2);
    }

    #[test]
    fn publication_order_refuses_a_generation_that_skips_a_registered_connection() {
        let mut coordinator = coordinator();
        let rendered = match render_snapshot(&VisibilityFlavor, snapshot(1, false), [FIRST]) {
            Ok(rendered) => rendered,
            Err(error) => panic!("render failed: {error}"),
        };
        let validated = match validate_rendered_snapshot(rendered) {
            Ok(validated) => validated,
            Err(error) => panic!("validation failed: {error}"),
        };

        match coordinator.publish(&validated) {
            Err(PublicationError::MissingConnectionOutput { connection }) => {
                assert_eq!(connection, SECOND);
            }
            Err(error) => panic!("unexpected error: {error}"),
            Ok(_pending) => panic!("a partial generation must not publish"),
        }
        assert_eq!(coordinator.generation(), 0);
    }

    #[test]
    fn publication_order_drops_the_routes_of_a_departed_connection() {
        let mut coordinator = coordinator();
        publish_and_commit(&mut coordinator, snapshot(1, true));
        assert!(hears(&coordinator, FIRST_SESSION, SECOND_SESSION));

        assert_eq!(coordinator.unregister(SECOND), Some(SECOND_SESSION));

        assert!(!hears(&coordinator, FIRST_SESSION, SECOND_SESSION));
        assert!(!hears(&coordinator, SECOND_SESSION, FIRST_SESSION));
        assert!(coordinator.committed_view(SECOND).is_none());
    }
}
