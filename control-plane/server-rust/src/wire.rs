use prost::Message;
use tonic::Status;

use crate::core_protocol as core;
use crate::protocol as legacy;
use crate::spaces_protocol as spaces;

const MAX_PROFILE_PAYLOAD_BYTES: usize = 1_048_576;

pub(crate) fn from_core_frame(frame: core::ClientFrame) -> Result<legacy::ClientFrame, Status> {
    use core::client_frame::Payload;

    let payload = match frame
        .payload
        .ok_or_else(|| Status::invalid_argument("ClientFrame has no payload"))?
    {
        Payload::OpenSession(message) => {
            legacy::client_frame::Payload::OpenSession(legacy::OpenSession {
                controller_id: message.controller_id,
                controller_instance_id: message.controller_instance_id,
                resume_token: message.resume_token,
                desired_state: message.desired_state.map(from_core_snapshot).transpose()?,
                profile: message.profile.map(from_core_profile),
            })
        }
        Payload::RenewLease(message) => {
            legacy::client_frame::Payload::RenewLease(legacy::RenewLease {
                session_token: message.session_token,
                desired_state_revision: message.desired_state_revision,
            })
        }
        Payload::SyncDesiredState(message) => {
            legacy::client_frame::Payload::SyncDesiredState(legacy::SyncDesiredState {
                session_token: message.session_token,
                desired_state: message.desired_state.map(from_core_snapshot).transpose()?,
            })
        }
        Payload::RegisterParticipant(message) => {
            legacy::client_frame::Payload::RegisterParticipant(legacy::RegisterParticipant {
                session_token: message.session_token,
                participant: message
                    .participant
                    .map(from_core_registration)
                    .transpose()?,
            })
        }
        Payload::SetParticipantSpec(message) => {
            legacy::client_frame::Payload::SetParticipantSpec(legacy::SetParticipantSpec {
                session_token: message.session_token,
                participant_id: message.participant_id,
                ownership_token: message.ownership_token,
                client_spec_revision: message.client_spec_revision,
                spec: Some(decode_payload(
                    message.profile_spec,
                    "participant profile spec",
                )?),
            })
        }
        Payload::ReleaseParticipant(message) => {
            legacy::client_frame::Payload::ReleaseParticipant(legacy::ReleaseParticipant {
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
            legacy::client_frame::Payload::CloseSession(legacy::CloseSession {
                session_token: message.session_token,
            })
        }
    };
    Ok(legacy::ClientFrame {
        request_id: frame.request_id,
        payload: Some(payload),
    })
}

fn from_profile_command(
    request_id: Vec<u8>,
    message: core::ProfileCommand,
) -> Result<legacy::ClientFrame, Status> {
    use spaces::command::Command;

    let command: spaces::Command = decode_payload(message.payload, "Spaces command")?;
    let payload = match command
        .command
        .ok_or_else(|| Status::invalid_argument("Spaces command has no command"))?
    {
        Command::ReplaceObservedSpaces(command) => {
            legacy::client_frame::Payload::ReplaceObservedSpaces(legacy::ReplaceObservedSpaces {
                session_token: message.session_token,
                observed_spaces_revision: command.observed_spaces_revision,
                space_keys: command.space_keys,
            })
        }
        Command::FetchSpace(command) => {
            legacy::client_frame::Payload::FetchSpace(legacy::FetchSpace {
                session_token: message.session_token,
                space_key: command.space_key,
            })
        }
    };
    Ok(legacy::ClientFrame {
        request_id,
        payload: Some(payload),
    })
}

fn from_core_snapshot(
    snapshot: core::DesiredStateSnapshot,
) -> Result<legacy::DesiredStateSnapshot, Status> {
    let profile_state: spaces::DesiredState = match snapshot.profile_state {
        Some(payload) => decode_payload(Some(payload), "Spaces desired state")?,
        None => spaces::DesiredState::default(),
    };
    Ok(legacy::DesiredStateSnapshot {
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
) -> Result<legacy::ParticipantRegistration, Status> {
    Ok(legacy::ParticipantRegistration {
        participant_id: registration.participant_id,
        registration_id: registration.registration_id,
        ownership_token: registration.ownership_token,
        client_spec_revision: registration.client_spec_revision,
        spec: Some(decode_payload(
            registration.profile_spec,
            "participant profile spec",
        )?),
    })
}

pub(crate) fn to_core_frame(frame: legacy::ServerFrame) -> Result<core::ServerFrame, String> {
    use legacy::server_frame::Payload;

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
                    mumble_join_token: message.mumble_join_token,
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
            spaces::event::Event::ObservedSpacesAccepted(transcode(message)?),
        ),
        Payload::SpaceSnapshot(message) => {
            profile_event(spaces::event::Event::SpaceSnapshot(transcode(message)?))
        }
        Payload::SpaceClosed(message) => {
            profile_event(spaces::event::Event::SpaceClosed(transcode(message)?))
        }
        Payload::FetchSpaceResult(message) => {
            profile_event(spaces::event::Event::FetchSpaceResult(transcode(message)?))
        }
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
            protobuf: spaces::Event { event: Some(event) }.encode_to_vec(),
        }),
    })
}

fn to_core_status(status: legacy::ParticipantStatus) -> core::ParticipantStatus {
    let profile_status = spaces::ParticipantStatus {
        applied_space_key: status.applied_space_key,
        self_mute: status.self_mute,
        self_deaf: status.self_deaf,
    };
    core::ParticipantStatus {
        mumble_connected: status.mumble_connected,
        accepted_spec_revision: status.accepted_spec_revision,
        applied_spec_revision: status.applied_spec_revision,
        published_generation: status.published_generation,
        application_error: status.application_error,
        profile_status: Some(core::ProfilePayload {
            protobuf: profile_status.encode_to_vec(),
        }),
    }
}

fn from_core_profile(profile: core::ProfileRef) -> legacy::ProfileRef {
    legacy::ProfileRef {
        profile_id: profile.profile_id,
        schema_version: profile.schema_version,
        descriptor_digest: profile.descriptor_digest,
    }
}

fn to_core_profile(profile: legacy::ProfileRef) -> core::ProfileRef {
    core::ProfileRef {
        profile_id: profile.profile_id,
        schema_version: profile.schema_version,
        descriptor_digest: profile.descriptor_digest,
    }
}

fn decode_payload<M>(payload: Option<core::ProfilePayload>, label: &str) -> Result<M, Status>
where
    M: Message + Default,
{
    let bytes = payload
        .ok_or_else(|| Status::invalid_argument(format!("{label} is missing")))?
        .protobuf;
    if bytes.len() > MAX_PROFILE_PAYLOAD_BYTES {
        return Err(Status::resource_exhausted(format!(
            "{label} exceeds {MAX_PROFILE_PAYLOAD_BYTES} bytes"
        )));
    }
    M::decode(bytes.as_slice())
        .map_err(|error| Status::invalid_argument(format!("invalid {label}: {error}")))
}

fn transcode<From, To>(message: From) -> Result<To, String>
where
    From: Message,
    To: Message + Default,
{
    To::decode(message.encode_to_vec().as_slice()).map_err(|error| error.to_string())
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
                        protobuf: vec![0; MAX_PROFILE_PAYLOAD_BYTES + 1],
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
