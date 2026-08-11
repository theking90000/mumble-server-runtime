use mumble_controller_spaces::{
    PayloadDecodeError, decode_command, decode_desired_state, decode_participant_spec,
    encode_event, encode_participant_status, protocol as spaces,
};
use tonic::Status;

use crate::actor_messages as model;
use crate::core_protocol as core;
pub(crate) fn from_core_frame(frame: core::ClientFrame) -> Result<model::ClientFrame, Status> {
    use core::client_frame::Payload;

    let payload = match frame
        .payload
        .ok_or_else(|| Status::invalid_argument("ClientFrame has no payload"))?
    {
        Payload::OpenSession(message) => {
            model::client_frame::Payload::OpenSession(model::OpenSession {
                controller_id: message.controller_id,
                controller_instance_id: message.controller_instance_id,
                resume_token: message.resume_token,
                desired_state: message.desired_state.map(from_core_snapshot).transpose()?,
                profile: message.profile.map(from_core_profile),
            })
        }
        Payload::RenewLease(message) => {
            model::client_frame::Payload::RenewLease(model::RenewLease {
                session_token: message.session_token,
                desired_state_revision: message.desired_state_revision,
            })
        }
        Payload::SyncDesiredState(message) => {
            model::client_frame::Payload::SyncDesiredState(model::SyncDesiredState {
                session_token: message.session_token,
                desired_state: message.desired_state.map(from_core_snapshot).transpose()?,
            })
        }
        Payload::RegisterParticipant(message) => {
            model::client_frame::Payload::RegisterParticipant(model::RegisterParticipant {
                session_token: message.session_token,
                participant: message
                    .participant
                    .map(from_core_registration)
                    .transpose()?,
            })
        }
        Payload::SetParticipantSpec(message) => {
            model::client_frame::Payload::SetParticipantSpec(model::SetParticipantSpec {
                session_token: message.session_token,
                participant_id: message.participant_id,
                ownership_token: message.ownership_token,
                client_spec_revision: message.client_spec_revision,
                spec: Some(to_actor_participant_spec(decode_participant_spec_payload(
                    message.profile_spec,
                )?)),
            })
        }
        Payload::ReleaseParticipant(message) => {
            model::client_frame::Payload::ReleaseParticipant(model::ReleaseParticipant {
                session_token: message.session_token,
                participant_id: message.participant_id,
                registration_id: message.registration_id,
                ownership_token: message.ownership_token,
            })
        }
        Payload::ProfileCommand(message) => {
            return from_profile_command(frame.request_id, message);
        }
        Payload::CloseSession(message) => {
            model::client_frame::Payload::CloseSession(model::CloseSession {
                session_token: message.session_token,
            })
        }
    };
    Ok(model::ClientFrame {
        request_id: frame.request_id,
        payload: Some(payload),
    })
}

fn from_profile_command(
    request_id: Vec<u8>,
    message: core::ProfileCommand,
) -> Result<model::ClientFrame, Status> {
    use spaces::command::Command;

    let command = decode_command_payload(message.payload)?;
    let payload = match command
        .command
        .ok_or_else(|| Status::invalid_argument("Spaces command has no command"))?
    {
        Command::ReplaceObservedSpaces(command) => {
            model::client_frame::Payload::ReplaceObservedSpaces(model::ReplaceObservedSpaces {
                session_token: message.session_token,
                observed_spaces_revision: command.observed_spaces_revision,
                space_keys: command.space_keys,
            })
        }
        Command::FetchSpace(command) => {
            model::client_frame::Payload::FetchSpace(model::FetchSpace {
                session_token: message.session_token,
                space_key: command.space_key,
            })
        }
    };
    Ok(model::ClientFrame {
        request_id,
        payload: Some(payload),
    })
}

fn from_core_snapshot(
    snapshot: core::DesiredStateSnapshot,
) -> Result<model::DesiredStateSnapshot, Status> {
    let profile_state = decode_desired_state_payload(snapshot.profile_state)?;
    Ok(model::DesiredStateSnapshot {
        desired_state_revision: snapshot.desired_state_revision,
        participants: snapshot
            .participants
            .into_iter()
            .map(from_core_registration)
            .collect::<Result<_, _>>()?,
        observed_space_keys: profile_state.observed_space_keys,
    })
}

fn from_core_registration(
    registration: core::ParticipantRegistration,
) -> Result<model::ParticipantRegistration, Status> {
    Ok(model::ParticipantRegistration {
        participant_id: registration.participant_id,
        registration_id: registration.registration_id,
        ownership_token: registration.ownership_token,
        client_spec_revision: registration.client_spec_revision,
        spec: Some(to_actor_participant_spec(decode_participant_spec_payload(
            registration.profile_spec,
        )?)),
    })
}

pub(crate) fn to_core_frame(frame: model::ServerFrame) -> Result<core::ServerFrame, String> {
    use model::server_frame::Payload;

    let payload = match frame
        .payload
        .ok_or_else(|| "ServerFrame has no payload".to_owned())?
    {
        Payload::SessionReady(message) => {
            core::server_frame::Payload::SessionReady(core::SessionReady {
                session_token: message.session_token,
                resume_token: message.resume_token,
                control_epoch: message.control_epoch,
                lease_duration: message.lease_duration,
                profile: message.profile.map(to_core_profile),
            })
        }
        Payload::DesiredStateReconciled(message) => {
            core::server_frame::Payload::DesiredStateReconciled(core::DesiredStateReconciled {
                desired_state_revision: message.desired_state_revision,
            })
        }
        Payload::ParticipantOwnershipGranted(message) => {
            core::server_frame::Payload::ParticipantOwnershipGranted(
                core::ParticipantOwnershipGranted {
                    participant_id: message.participant_id,
                    registration_id: message.registration_id,
                    ownership_token: message.ownership_token,
                    client_spec_revision: message.client_spec_revision,
                    accepted_spec_revision: message.accepted_spec_revision,
                    applied_spec_revision: message.applied_spec_revision,
                    published_generation: message.published_generation,
                    connection_credential: message.connection_credential,
                },
            )
        }
        Payload::ParticipantOwnershipRevoked(message) => {
            core::server_frame::Payload::ParticipantOwnershipRevoked(
                core::ParticipantOwnershipRevoked {
                    participant_id: message.participant_id,
                    registration_id: message.registration_id,
                    reason: message.reason,
                },
            )
        }
        Payload::ParticipantSpecAccepted(message) => {
            core::server_frame::Payload::ParticipantSpecAccepted(core::ParticipantSpecAccepted {
                participant_id: message.participant_id,
                client_spec_revision: message.client_spec_revision,
                accepted_spec_revision: message.accepted_spec_revision,
                applied_spec_revision: message.applied_spec_revision,
                published_generation: message.published_generation,
            })
        }
        Payload::ParticipantStatusChanged(message) => {
            core::server_frame::Payload::ParticipantStatusChanged(core::ParticipantStatusChanged {
                participant_id: message.participant_id,
                status: message.status.map(to_core_status),
            })
        }
        Payload::ObservedSpacesAccepted(message) => profile_event(
            spaces::event::Event::ObservedSpacesAccepted(spaces::ObservedSpacesAccepted {
                observed_spaces_revision: message.observed_spaces_revision,
            }),
        ),
        Payload::SpaceSnapshot(message) => profile_event(spaces::event::Event::SpaceSnapshot(
            to_spaces_snapshot(message),
        )),
        Payload::SpaceClosed(message) => {
            profile_event(spaces::event::Event::SpaceClosed(spaces::SpaceClosed {
                space_key: message.space_key,
                incarnation_id: message.incarnation_id,
                final_space_revision: message.final_space_revision,
            }))
        }
        Payload::FetchSpaceResult(message) => profile_event(
            spaces::event::Event::FetchSpaceResult(to_spaces_fetch(message)),
        ),
        Payload::ResyncRequired(message) => {
            core::server_frame::Payload::ResyncRequired(core::ResyncRequired {
                reason: message.reason,
            })
        }
        Payload::CommandRejected(message) => {
            core::server_frame::Payload::CommandRejected(core::CommandRejected {
                code: message.code,
                message: message.message,
            })
        }
        Payload::SessionClosing(message) => {
            core::server_frame::Payload::SessionClosing(core::SessionClosing {
                reason: message.reason,
            })
        }
    };
    Ok(core::ServerFrame {
        request_id: frame.request_id,
        payload: Some(payload),
    })
}

fn profile_event(event: spaces::event::Event) -> core::server_frame::Payload {
    core::server_frame::Payload::ProfileEvent(core::ProfileEvent {
        payload: Some(core::ProfilePayload {
            protobuf: encode_event(event),
        }),
    })
}

fn to_core_status(status: model::ParticipantStatus) -> core::ParticipantStatus {
    let profile_status = spaces::ParticipantStatus {
        applied_space_key: status.applied_space_key,
        self_mute: status.self_mute,
        self_deaf: status.self_deaf,
    };
    core::ParticipantStatus {
        connected: status.connected,
        accepted_spec_revision: status.accepted_spec_revision,
        applied_spec_revision: status.applied_spec_revision,
        published_generation: status.published_generation,
        application_error: status.application_error,
        profile_status: Some(core::ProfilePayload {
            protobuf: encode_participant_status(profile_status),
        }),
    }
}

fn from_core_profile(profile: core::ProfileRef) -> model::ProfileRef {
    model::ProfileRef {
        profile_id: profile.profile_id,
        schema_version: profile.schema_version,
        descriptor_digest: profile.descriptor_digest,
    }
}

fn to_core_profile(profile: model::ProfileRef) -> core::ProfileRef {
    core::ProfileRef {
        profile_id: profile.profile_id,
        schema_version: profile.schema_version,
        descriptor_digest: profile.descriptor_digest,
    }
}

fn decode_command_payload(
    payload: Option<core::ProfilePayload>,
) -> Result<spaces::Command, Status> {
    decode_command(payload.as_ref().map(|payload| payload.protobuf.as_slice()))
        .map_err(payload_status)
}

fn decode_desired_state_payload(
    payload: Option<core::ProfilePayload>,
) -> Result<spaces::DesiredState, Status> {
    decode_desired_state(payload.as_ref().map(|payload| payload.protobuf.as_slice()))
        .map_err(payload_status)
}

fn decode_participant_spec_payload(
    payload: Option<core::ProfilePayload>,
) -> Result<spaces::ParticipantSpec, Status> {
    decode_participant_spec(payload.as_ref().map(|payload| payload.protobuf.as_slice()))
        .map_err(payload_status)
}

fn to_actor_participant_spec(spec: spaces::ParticipantSpec) -> model::ParticipantSpec {
    model::ParticipantSpec {
        space_key: spec.space_key,
        display_name: spec.display_name,
        server_mute: spec.server_mute,
        server_deaf: spec.server_deaf,
    }
}

fn payload_status(error: PayloadDecodeError) -> Status {
    if error.is_too_large() {
        Status::resource_exhausted(error.to_string())
    } else {
        Status::invalid_argument(error.to_string())
    }
}

fn to_spaces_snapshot(message: model::SpaceSnapshot) -> spaces::SpaceSnapshot {
    spaces::SpaceSnapshot {
        space_key: message.space_key,
        incarnation_id: message.incarnation_id,
        space_revision: message.space_revision,
        participants: message
            .participants
            .into_iter()
            .map(|participant| spaces::SpaceParticipant {
                participant_id: participant.participant_id,
                display_name: participant.display_name,
                server_mute: participant.server_mute,
                server_deaf: participant.server_deaf,
                connected: participant.connected,
            })
            .collect(),
        published_generation: message.published_generation,
    }
}

fn to_spaces_fetch(message: model::FetchSpaceResult) -> spaces::FetchSpaceResult {
    let result = message.result.map(|result| match result {
        model::fetch_space_result::Result::Snapshot(snapshot) => {
            spaces::fetch_space_result::Result::Snapshot(to_spaces_snapshot(snapshot))
        }
        model::fetch_space_result::Result::Absent(absent) => {
            spaces::fetch_space_result::Result::Absent(spaces::SpaceAbsent {
                space_key: absent.space_key,
            })
        }
    });
    spaces::FetchSpaceResult { result }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_an_untyped_profile_command_before_the_actor() {
        let frame = core::ClientFrame {
            request_id: vec![1],
            payload: Some(core::client_frame::Payload::ProfileCommand(
                core::ProfileCommand {
                    session_token: vec![2],
                    payload: None,
                },
            )),
        };
        let Err(error) = from_core_frame(frame) else {
            panic!("missing payload must fail");
        };
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn rejects_a_profile_payload_over_the_explicit_limit() {
        let frame = core::ClientFrame {
            request_id: vec![1],
            payload: Some(core::client_frame::Payload::ProfileCommand(
                core::ProfileCommand {
                    session_token: vec![2],
                    payload: Some(core::ProfilePayload {
                        protobuf: vec![0; mumble_controller_spaces::MAX_PROFILE_PAYLOAD_BYTES + 1],
                    }),
                },
            )),
        };
        let Err(error) = from_core_frame(frame) else {
            panic!("oversized payload must fail");
        };
        assert_eq!(error.code(), tonic::Code::ResourceExhausted);
    }
}
