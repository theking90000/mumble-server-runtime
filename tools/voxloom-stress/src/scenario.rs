use mumble_server_runtime_protocol::ControlMessage;
use mumble_server_runtime_protocol::messages::tcp;

use crate::client::Model;
use crate::config::ScenarioKind;

#[derive(Debug)]
pub enum Scenario {
    Connect,
    Arena(Arena),
}

impl Scenario {
    pub fn new(kind: ScenarioKind, client_number: usize) -> Scenario {
        match kind {
            ScenarioKind::Connect => Scenario::Connect,
            ScenarioKind::Arena => Scenario::Arena(Arena::new(client_number)),
        }
    }

    /// Return the next interaction only when the local Mumble model proves that
    /// its prerequisite state has arrived.
    pub fn next(&mut self, model: &Model) -> Option<ControlMessage> {
        match self {
            Scenario::Connect => None,
            Scenario::Arena(arena) => arena.next(model),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Red,
    Blue,
}

impl Side {
    const fn lobby_name(self) -> &'static str {
        match self {
            Side::Red => "Red Team",
            Side::Blue => "Blue Team",
        }
    }

    const fn arena_name(self) -> &'static str {
        match self {
            Side::Red => "Red Base",
            Side::Blue => "Blue Base",
        }
    }

    const fn opposite(self) -> Side {
        match self {
            Side::Red => Side::Blue,
            Side::Blue => Side::Red,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    ChooseLobby,
    EnterArena,
    BecomeSpectator,
    ChooseArena,
    ReturnLobby,
}

#[derive(Debug)]
pub struct Arena {
    side: Side,
    phase: Phase,
}

impl Arena {
    fn new(client_number: usize) -> Arena {
        Arena {
            side: if client_number.is_multiple_of(2) {
                Side::Red
            } else {
                Side::Blue
            },
            phase: Phase::ChooseLobby,
        }
    }

    fn next(&mut self, model: &Model) -> Option<ControlMessage> {
        let session = model.session?;
        let target = match self.phase {
            Phase::ChooseLobby => {
                let channel = model.channel_named(self.side.lobby_name())?;
                self.phase = Phase::EnterArena;
                channel
            }
            Phase::EnterArena => {
                if model.self_channel_name()? != self.side.lobby_name() {
                    return None;
                }
                let channel = model.channel_named("> Enter the Arena")?;
                self.phase = Phase::BecomeSpectator;
                channel
            }
            Phase::BecomeSpectator => {
                if model.self_channel_name()? != self.side.arena_name() {
                    return None;
                }
                let channel = model.channel_named("Neutral Ground")?;
                self.side = self.side.opposite();
                self.phase = Phase::ChooseArena;
                channel
            }
            Phase::ChooseArena => {
                if model.self_channel_name()? != "Observation Deck" {
                    return None;
                }
                let channel = model.channel_named(self.side.arena_name())?;
                self.phase = Phase::ReturnLobby;
                channel
            }
            Phase::ReturnLobby => {
                if model.self_channel_name()? != self.side.arena_name() {
                    return None;
                }
                let channel = model.channel_named("< Back to the Lobby")?;
                self.phase = Phase::ChooseLobby;
                channel
            }
        };

        // REF: references/mumble/src/Mumble.proto:UserState fields session and channel_id.
        Some(ControlMessage::UserState(tcp::UserState {
            session: Some(session),
            channel_id: Some(target),
            ..Default::default()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{Channel, User};

    fn model_in(channel_name: &str, channels: &[&str]) -> Model {
        let mut model = Model {
            session: Some(7),
            ..Model::default()
        };
        for (index, name) in channels.iter().enumerate() {
            let id = u32::try_from(index + 1).unwrap_or_default();
            model.channels.insert(
                id,
                Channel {
                    name: (*name).to_owned(),
                    parent: Some(0),
                },
            );
            if *name == channel_name {
                model.users.insert(
                    7,
                    User {
                        name: "stress-0".to_owned(),
                        channel: id,
                    },
                );
            }
        }
        model
    }

    fn target(message: ControlMessage) -> Option<u32> {
        match message {
            ControlMessage::UserState(state) => state.channel_id,
            _ => None,
        }
    }

    #[test]
    fn arena_waits_for_each_observed_transition() {
        let lobby = ["Red Team", "Blue Team", "> Enter the Arena"];
        let mut scenario = Arena::new(0);

        let first = scenario
            .next(&model_in("Mumble Server Runtime Arena", &lobby))
            .and_then(target);
        assert_eq!(first, Some(1));

        assert!(scenario.next(&model_in("Mumble Server Runtime Arena", &lobby)).is_none());
        let enter = scenario
            .next(&model_in("Red Team", &lobby))
            .and_then(target);
        assert_eq!(enter, Some(3));
    }

    #[test]
    fn connect_scenario_never_mutates_client_state() {
        let mut scenario = Scenario::new(ScenarioKind::Connect, 0);
        assert!(scenario.next(&Model::default()).is_none());
    }
}
