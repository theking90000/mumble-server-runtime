//! Built-in Controller profile that renders one root channel and audio domain per named Space.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mumble_controller_host::runtime::{
    Audience, ConnectionId, DomainId, Narrow, Occupant, ReconcileReport, Reply, Scope, ScopeSet,
    ShardBuilder, ShardHandle, ShardLogic, UserFlags, VoiceEvent,
};
use mumble_controller_host::{SnapshotPublisher, SnapshotReader, VersionedSnapshot};

/// Stable business identity of one named Space.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SpaceKey(String);

impl SpaceKey {
    /// Validate a Space key before it enters profile state.
    pub fn new(value: String) -> Result<Self, SpacesValidationError> {
        if value.is_empty() || value.len() > 128 {
            return Err(SpacesValidationError::InvalidSpaceKey);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Spaces-owned desired state for one logical participant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantSpec {
    space_key: SpaceKey,
    display_name: String,
    server_mute: bool,
    server_deaf: bool,
}

impl ParticipantSpec {
    /// Validate a complete participant payload before profile state changes.
    pub fn new(
        space_key: String,
        display_name: String,
        server_mute: bool,
        server_deaf: bool,
    ) -> Result<Self, SpacesValidationError> {
        let space_key = SpaceKey::new(space_key)?;
        if display_name.is_empty() || display_name.chars().count() > 64 {
            return Err(SpacesValidationError::InvalidDisplayName);
        }
        Ok(Self {
            space_key,
            display_name,
            server_mute,
            server_deaf,
        })
    }

    #[must_use]
    pub fn space_key(&self) -> &SpaceKey {
        &self.space_key
    }

    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    #[must_use]
    pub fn server_mute(&self) -> bool {
        self.server_mute
    }

    #[must_use]
    pub fn server_deaf(&self) -> bool {
        self.server_deaf
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SpacesValidationError {
    #[error("space_key must contain between 1 and 128 bytes")]
    InvalidSpaceKey,
    #[error("display_name must contain between 1 and 64 characters")]
    InvalidDisplayName,
}

/// Profile-owned state for one materialized Space.
pub struct MaterializedSpace {
    handle: ShardHandle,
    incarnation_id: Vec<u8>,
    space_revision: u64,
    published_generation: u64,
    desired: SnapshotPublisher<RenderState>,
    render_history: BTreeMap<u64, BTreeMap<String, u64>>,
    close_deadline: Option<Instant>,
}

impl MaterializedSpace {
    #[must_use]
    pub fn new(
        handle: ShardHandle,
        incarnation_id: Vec<u8>,
        desired: SnapshotPublisher<RenderState>,
    ) -> Self {
        Self {
            handle,
            incarnation_id,
            space_revision: 0,
            published_generation: 0,
            desired,
            render_history: BTreeMap::new(),
            close_deadline: None,
        }
    }

    #[must_use]
    pub fn shard_handle(&self) -> &ShardHandle {
        &self.handle
    }

    #[must_use]
    pub fn incarnation_id(&self) -> &[u8] {
        &self.incarnation_id
    }

    #[must_use]
    pub fn revision(&self) -> u64 {
        self.space_revision
    }

    #[must_use]
    pub fn published_generation(&self) -> u64 {
        self.published_generation
    }

    #[must_use]
    pub fn close_deadline(&self) -> Option<Instant> {
        self.close_deadline
    }

    pub fn refresh(
        &mut self,
        application_revision: u64,
        space_key: &str,
        participants: Vec<RenderParticipant>,
        now: Instant,
        empty_space_grace: Duration,
    ) {
        let revisions = participants
            .iter()
            .map(|participant| {
                (
                    participant.participant_id.clone(),
                    participant.accepted_revision,
                )
            })
            .collect();
        self.space_revision = self.space_revision.saturating_add(1);
        self.close_deadline = if participants.is_empty() {
            self.close_deadline.or(Some(now + empty_space_grace))
        } else {
            None
        };
        self.render_history.insert(application_revision, revisions);
        self.desired.publish(Arc::new(RenderState {
            application_revision,
            space_key: space_key.to_owned(),
            participants,
        }));
        self.handle.wake();
    }

    pub fn reconcile(
        &mut self,
        application_revision: u64,
        report: &ReconcileReport,
    ) -> Option<SpaceReconciliation> {
        let revisions = self.render_history.get(&application_revision).cloned()?;
        let refusal = report.refused.as_ref().map(ToString::to_string);
        if refusal.is_none() {
            self.published_generation = report.version;
        }
        self.render_history
            .retain(|revision, _| *revision > application_revision);
        Some(SpaceReconciliation {
            revisions,
            refusal,
            published_generation: report.version,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceReconciliation {
    pub revisions: BTreeMap<String, u64>,
    pub refusal: Option<String>,
    pub published_generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpaceEvent {
    pub connection: ConnectionId,
    pub kind: SpaceEventKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceEventKind {
    Connected,
    Disconnected,
    SelfState { self_mute: bool, self_deaf: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceReport {
    pub space_key: String,
    pub event: SpaceEvent,
}

pub trait SpaceReporter: Send + 'static {
    fn try_report(&self, report: SpaceReport) -> Result<(), SpaceReport>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderParticipant {
    pub participant_id: String,
    pub connection: Option<ConnectionId>,
    pub display_name: String,
    pub server_mute: bool,
    pub server_deaf: bool,
    pub self_mute: bool,
    pub self_deaf: bool,
    pub accepted_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderState {
    pub application_revision: u64,
    pub space_key: String,
    pub participants: Vec<RenderParticipant>,
}

impl VersionedSnapshot for RenderState {
    fn revision(&self) -> u64 {
        self.application_revision
    }
}

pub struct ControllerSpaceLogic<R> {
    space_key: String,
    desired: SnapshotReader<RenderState>,
    reporter: R,
    self_state: BTreeMap<ConnectionId, (bool, bool)>,
    undelivered_departures: Vec<ConnectionId>,
}

impl<R: SpaceReporter> ControllerSpaceLogic<R> {
    pub fn new(space_key: String, desired: SnapshotReader<RenderState>, reporter: R) -> Self {
        Self {
            space_key,
            desired,
            reporter,
            self_state: BTreeMap::new(),
            undelivered_departures: Vec::new(),
        }
    }

    /// The declared self-state of a connection, seeded from the desired snapshot.
    ///
    /// `RequestedSelfState` carries one `Option` per flag, so a client that changes
    /// only one of them relies on the other keeping its published value. Defaulting
    /// a fresh entry to `(false, false)` would silently clear the other flag after a
    /// migration, which drops this shard's entry while the desired state keeps it.
    fn entry(&mut self, connection: ConnectionId) -> &mut (bool, bool) {
        let published = self
            .desired
            .current()
            .participants
            .iter()
            .find(|participant| participant.connection == Some(connection))
            .map_or((false, false), |participant| {
                (participant.self_mute, participant.self_deaf)
            });
        self.self_state.entry(connection).or_insert(published)
    }

    /// Report a Space event, retaining departures the actor could not accept.
    ///
    /// A dropped `Connected` or `SelfState` is corrected by the next render.
    /// A dropped `Disconnected` is not: the actor would keep the participant
    /// bound to a dead connection, keep rendering it, and never let the Space
    /// go empty. Departures are therefore retried, bounded by live connections.
    fn report(&mut self, event: SpaceEvent) {
        let report = SpaceReport {
            space_key: self.space_key.clone(),
            event,
        };
        if let Err(report) = self.reporter.try_report(report) {
            if report.event.kind == SpaceEventKind::Disconnected {
                self.undelivered_departures.push(report.event.connection);
            }
            eprintln!("mumble-controller-spaces: dropping an event under backpressure");
        }
    }

    fn flush_departures(&mut self) {
        let pending = std::mem::take(&mut self.undelivered_departures);
        for connection in pending {
            self.report(SpaceEvent {
                connection,
                kind: SpaceEventKind::Disconnected,
            });
        }
    }
}

impl<R: SpaceReporter> ShardLogic for ControllerSpaceLogic<R> {
    fn render(&mut self, out: &mut ShardBuilder<'_>) {
        self.flush_departures();
        let desired = self.desired.latest();

        let root = out.root(&desired.space_key);
        let mut audible = Vec::new();
        for participant in &desired.participants {
            let Some(connection) = participant.connection else {
                continue;
            };
            let user = out.user(
                root,
                Occupant::Connection(connection),
                &participant.display_name,
                Narrow::Same,
            );
            let (self_mute, self_deaf) = self
                .self_state
                .get(&connection)
                .copied()
                .unwrap_or((participant.self_mute, participant.self_deaf));
            out.user_flags(
                user,
                UserFlags {
                    mute: participant.server_mute,
                    deaf: participant.server_deaf,
                    self_mute,
                    self_deaf,
                    ..UserFlags::default()
                },
            );
            audible.push(connection);
        }
        out.audio_domain(DomainId(0), &audible);
    }

    fn observation(&mut self, _connection: ConnectionId) -> ScopeSet {
        ScopeSet::new(&[Scope::ROOT]).unwrap_or(ScopeSet::NONE)
    }

    fn observe(&mut self, event: &VoiceEvent, out: &mut Reply) {
        self.flush_departures();
        match event {
            VoiceEvent::Connected { connection } => {
                self.report(SpaceEvent {
                    connection: *connection,
                    kind: SpaceEventKind::Connected,
                });
            }
            VoiceEvent::Disconnected { connection, .. } => {
                self.self_state.remove(connection);
                self.report(SpaceEvent {
                    connection: *connection,
                    kind: SpaceEventKind::Disconnected,
                });
            }
            VoiceEvent::Migrated { connection, .. } => {
                self.self_state.remove(connection);
            }
            VoiceEvent::RequestedSelfState {
                connection,
                self_mute,
                self_deaf,
            } => {
                let current = self.entry(*connection);
                if let Some(value) = self_mute {
                    current.0 = *value;
                }
                if let Some(value) = self_deaf {
                    current.1 = *value;
                }
                let (self_mute, self_deaf) = *current;
                self.report(SpaceEvent {
                    connection: *connection,
                    kind: SpaceEventKind::SelfState {
                        self_mute,
                        self_deaf,
                    },
                });
            }
            VoiceEvent::Said {
                connection,
                to,
                text,
            } => {
                let audience = match to {
                    Audience::Channel(channel) => Audience::Channel(*channel),
                    Audience::Tree(channel) => Audience::Tree(*channel),
                    Audience::User(user) => Audience::User(*user),
                };
                out.relay(*connection, audience, text);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::Mutex;

    use mumble_controller_host::runtime::{ChannelKey, Spoken};

    use super::*;

    #[test]
    fn participant_specs_are_validated_by_the_spaces_profile() {
        assert_eq!(
            ParticipantSpec::new(String::new(), "Alice".to_owned(), false, false),
            Err(SpacesValidationError::InvalidSpaceKey)
        );
        assert_eq!(
            ParticipantSpec::new("lobby".to_owned(), String::new(), false, false),
            Err(SpacesValidationError::InvalidDisplayName)
        );
        assert!(ParticipantSpec::new("lobby".to_owned(), "Alice".to_owned(), true, false).is_ok());
    }

    #[derive(Clone)]
    struct TestReporter {
        capacity: usize,
        events: Arc<Mutex<VecDeque<SpaceReport>>>,
    }

    impl TestReporter {
        fn new(capacity: usize) -> Self {
            Self {
                capacity,
                events: Arc::new(Mutex::new(VecDeque::new())),
            }
        }

        fn pop(&self) -> Option<SpaceReport> {
            self.events.lock().expect("event queue lock").pop_front()
        }

        fn is_empty(&self) -> bool {
            self.events.lock().expect("event queue lock").is_empty()
        }
    }

    impl SpaceReporter for TestReporter {
        fn try_report(&self, report: SpaceReport) -> Result<(), SpaceReport> {
            let mut events = self.events.lock().expect("event queue lock");
            if events.len() >= self.capacity {
                return Err(report);
            }
            events.push_back(report);
            Ok(())
        }
    }

    fn participant(
        connection: ConnectionId,
        self_mute: bool,
        self_deaf: bool,
    ) -> RenderParticipant {
        RenderParticipant {
            participant_id: "alice".to_owned(),
            connection: Some(connection),
            display_name: "Alice".to_owned(),
            server_mute: false,
            server_deaf: false,
            self_mute,
            self_deaf,
            accepted_revision: 1,
        }
    }

    fn logic_with(
        participants: Vec<RenderParticipant>,
        actor_capacity: usize,
    ) -> (
        ControllerSpaceLogic<TestReporter>,
        mumble_controller_host::SnapshotPublisher<RenderState>,
        TestReporter,
    ) {
        let (desired, receiver) = mumble_controller_host::snapshot_channel(Arc::new(RenderState {
            application_revision: 1,
            space_key: "lobby".to_owned(),
            participants,
        }));
        let events = TestReporter::new(actor_capacity);
        let logic = ControllerSpaceLogic::new("lobby".to_owned(), receiver, events.clone());
        (logic, desired, events)
    }

    /// A partial `RequestedSelfState` must not clear the flag it does not carry.
    ///
    /// This shard holds no entry for the connection right after a migration, while
    /// the desired snapshot still declares the published flags. Seeding a fresh
    /// entry with `(false, false)` un-mutes a client that only asked to deafen.
    #[test]
    fn a_partial_self_state_keeps_the_flag_it_does_not_carry() {
        let (mut logic, _desired, events) =
            logic_with(vec![participant(ConnectionId(7), true, false)], 4);
        let mut reply = Reply::default();
        logic.observe(
            &VoiceEvent::RequestedSelfState {
                connection: ConnectionId(7),
                self_mute: None,
                self_deaf: Some(true),
            },
            &mut reply,
        );

        let event = events.pop().expect("a self-state event").event;
        assert_eq!(
            event,
            SpaceEvent {
                connection: ConnectionId(7),
                kind: SpaceEventKind::SelfState {
                    self_mute: true,
                    self_deaf: true,
                },
            }
        );
    }

    /// A departure refused by a full actor mailbox must be retried.
    ///
    /// Losing it is not a lost notification but lost state: the actor would keep
    /// the participant bound to a dead connection, keep rendering it, and never
    /// let the Space go empty.
    #[test]
    fn a_departure_refused_under_backpressure_is_retried() {
        let (mut logic, _desired, events) =
            logic_with(vec![participant(ConnectionId(7), false, false)], 1);
        let mut reply = Reply::default();
        logic.observe(
            &VoiceEvent::Connected {
                connection: ConnectionId(7),
            },
            &mut reply,
        );
        logic.observe(
            &VoiceEvent::Disconnected {
                connection: ConnectionId(7),
                reason: "test departure".to_owned(),
            },
            &mut reply,
        );

        let event = events.pop().expect("the arrival").event;
        assert_eq!(
            event,
            SpaceEvent {
                connection: ConnectionId(7),
                kind: SpaceEventKind::Connected,
            }
        );
        assert!(
            events.is_empty(),
            "the departure could not fit in the mailbox"
        );

        logic.observe(
            &VoiceEvent::Connected {
                connection: ConnectionId(8),
            },
            &mut reply,
        );
        let event = events.pop().expect("the retried departure").event;
        assert_eq!(
            event,
            SpaceEvent {
                connection: ConnectionId(7),
                kind: SpaceEventKind::Disconnected,
            }
        );
    }

    #[test]
    fn room_text_is_relayed_to_the_exact_runtime_audience() {
        let initial = Arc::new(RenderState {
            application_revision: 1,
            space_key: "lobby".to_owned(),
            participants: Vec::new(),
        });
        let (_desired, receiver) = mumble_controller_host::snapshot_channel(initial);
        let mut logic =
            ControllerSpaceLogic::new("lobby".to_owned(), receiver, TestReporter::new(1));
        let mut reply = Reply::default();
        logic.observe(
            &VoiceEvent::Said {
                connection: ConnectionId(7),
                to: Audience::Channel(ChannelKey::ROOT),
                text: "hello".to_owned(),
            },
            &mut reply,
        );

        assert_eq!(
            reply.drain_spoken(),
            vec![Spoken {
                from: Some(ConnectionId(7)),
                to: Audience::Channel(ChannelKey::ROOT),
                text: "hello".to_owned(),
            }]
        );
    }
}
