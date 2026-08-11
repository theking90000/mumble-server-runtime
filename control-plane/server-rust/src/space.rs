use std::collections::BTreeMap;

use mumble_controller_host::{SnapshotReader, VersionedSnapshot};
use mumble_server_runtime_shard::{
    Audience, ConnectionId, DomainId, Narrow, Occupant, Reply, Scope, ScopeSet, ShardBuilder,
    ShardLogic, UserFlags, VoiceEvent,
};
use tokio::sync::mpsc;

use crate::actor::{ActorCommand, SpaceEvent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenderParticipant {
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
pub(crate) struct RenderState {
    pub application_revision: u64,
    pub space_key: String,
    pub participants: Vec<RenderParticipant>,
}

impl VersionedSnapshot for RenderState {
    fn revision(&self) -> u64 {
        self.application_revision
    }
}

pub(crate) struct ControllerSpaceLogic {
    space_key: String,
    desired: SnapshotReader<RenderState>,
    actor: mpsc::Sender<ActorCommand>,
    self_state: BTreeMap<ConnectionId, (bool, bool)>,
    undelivered_departures: Vec<ConnectionId>,
}

impl ControllerSpaceLogic {
    pub(crate) fn new(
        space_key: String,
        desired: SnapshotReader<RenderState>,
        actor: mpsc::Sender<ActorCommand>,
    ) -> Self {
        Self {
            space_key,
            desired,
            actor,
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
        if let Err(error) = self.actor.try_send(ActorCommand::SpaceEvent {
            space_key: self.space_key.clone(),
            event,
        }) {
            if let SpaceEvent::Disconnected(connection) = event {
                self.undelivered_departures.push(connection);
            }
            eprintln!(
                "mumble-controller-server: dropping a Space event under backpressure: {error}"
            );
        }
    }

    fn flush_departures(&mut self) {
        let pending = std::mem::take(&mut self.undelivered_departures);
        for connection in pending {
            self.report(SpaceEvent::Disconnected(connection));
        }
    }
}

impl ShardLogic for ControllerSpaceLogic {
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
                self.report(SpaceEvent::Connected(*connection));
            }
            VoiceEvent::Disconnected { connection, .. } => {
                self.self_state.remove(connection);
                self.report(SpaceEvent::Disconnected(*connection));
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
                self.report(SpaceEvent::SelfState {
                    connection: *connection,
                    self_mute,
                    self_deaf,
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

    use std::sync::Arc;

    use mumble_server_runtime_shard::{ChannelKey, Spoken};

    use super::*;

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
        ControllerSpaceLogic,
        mumble_controller_host::SnapshotPublisher<RenderState>,
        mpsc::Receiver<ActorCommand>,
    ) {
        let (desired, receiver) = mumble_controller_host::snapshot_channel(Arc::new(RenderState {
            application_revision: 1,
            space_key: "lobby".to_owned(),
            participants,
        }));
        let (actor, events) = mpsc::channel(actor_capacity);
        let logic = ControllerSpaceLogic::new("lobby".to_owned(), receiver, actor);
        (logic, desired, events)
    }

    /// A partial `RequestedSelfState` must not clear the flag it does not carry.
    ///
    /// This shard holds no entry for the connection right after a migration, while
    /// the desired snapshot still declares the published flags. Seeding a fresh
    /// entry with `(false, false)` un-mutes a client that only asked to deafen.
    #[test]
    fn a_partial_self_state_keeps_the_flag_it_does_not_carry() {
        let (mut logic, _desired, mut events) =
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

        let ActorCommand::SpaceEvent { event, .. } = events.try_recv().expect("a self-state event")
        else {
            panic!("the logic must report a Space event")
        };
        assert_eq!(
            event,
            SpaceEvent::SelfState {
                connection: ConnectionId(7),
                self_mute: true,
                self_deaf: true,
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
        let (mut logic, _desired, mut events) =
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

        let ActorCommand::SpaceEvent { event, .. } = events.try_recv().expect("the arrival") else {
            panic!("the logic must report a Space event")
        };
        assert_eq!(event, SpaceEvent::Connected(ConnectionId(7)));
        assert!(
            events.try_recv().is_err(),
            "the departure could not fit in the mailbox"
        );

        logic.observe(
            &VoiceEvent::Connected {
                connection: ConnectionId(8),
            },
            &mut reply,
        );
        let ActorCommand::SpaceEvent { event, .. } =
            events.try_recv().expect("the retried departure")
        else {
            panic!("the logic must report a Space event")
        };
        assert_eq!(event, SpaceEvent::Disconnected(ConnectionId(7)));
    }

    #[test]
    fn room_text_is_relayed_to_the_exact_runtime_audience() {
        let initial = Arc::new(RenderState {
            application_revision: 1,
            space_key: "lobby".to_owned(),
            participants: Vec::new(),
        });
        let (_desired, receiver) = mumble_controller_host::snapshot_channel(initial);
        let (actor, _events) = mpsc::channel(1);
        let mut logic = ControllerSpaceLogic::new("lobby".to_owned(), receiver, actor);
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
