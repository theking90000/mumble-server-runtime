use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use mumble_server_runtime_shard::{
    Audience, ConnectionId, DomainId, Narrow, Occupant, Reply, Scope, ScopeSet, ShardBuilder,
    ShardLogic, UserFlags, VoiceEvent,
};
use tokio::sync::{mpsc, watch};

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

pub(crate) struct ControllerSpaceLogic {
    space_key: String,
    desired: watch::Receiver<Arc<RenderState>>,
    rendered_application_revision: Arc<AtomicU64>,
    actor: mpsc::Sender<ActorCommand>,
    self_state: BTreeMap<ConnectionId, (bool, bool)>,
}

impl ControllerSpaceLogic {
    pub(crate) fn new(
        space_key: String,
        desired: watch::Receiver<Arc<RenderState>>,
        rendered_application_revision: Arc<AtomicU64>,
        actor: mpsc::Sender<ActorCommand>,
    ) -> Self {
        Self {
            space_key,
            desired,
            rendered_application_revision,
            actor,
            self_state: BTreeMap::new(),
        }
    }

    fn report(&self, event: SpaceEvent) {
        if let Err(error) = self.actor.try_send(ActorCommand::SpaceEvent {
            space_key: self.space_key.clone(),
            event,
        }) {
            eprintln!(
                "mumble-controller-server: dropping a Space event under backpressure: {error}"
            );
        }
    }
}

impl ShardLogic for ControllerSpaceLogic {
    fn render(&mut self, out: &mut ShardBuilder<'_>) {
        let desired = Arc::clone(&self.desired.borrow_and_update());
        self.rendered_application_revision
            .store(desired.application_revision, Ordering::SeqCst);

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
                let current = self.self_state.entry(*connection).or_default();
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
    use mumble_server_runtime_shard::{ChannelKey, Spoken};

    use super::*;

    #[test]
    fn room_text_is_relayed_to_the_exact_runtime_audience() {
        let initial = Arc::new(RenderState {
            application_revision: 1,
            space_key: "lobby".to_owned(),
            participants: Vec::new(),
        });
        let (_desired, receiver) = watch::channel(initial);
        let (actor, _events) = mpsc::channel(1);
        let mut logic = ControllerSpaceLogic::new(
            "lobby".to_owned(),
            receiver,
            Arc::new(AtomicU64::new(0)),
            actor,
        );
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
