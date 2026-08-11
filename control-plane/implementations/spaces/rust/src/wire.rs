use prost::Message;

use crate::protocol::{Command, DesiredState, Event, ParticipantSpec, ParticipantStatus, event};

pub const MAX_PROFILE_PAYLOAD_BYTES: usize = 1_048_576;

#[derive(Debug, thiserror::Error)]
pub enum PayloadDecodeError {
    #[error("{label} is missing")]
    Missing { label: &'static str },
    #[error("{label} exceeds {limit} bytes")]
    TooLarge { label: &'static str, limit: usize },
    #[error("invalid {label}: {source}")]
    Invalid {
        label: &'static str,
        #[source]
        source: prost::DecodeError,
    },
}

impl PayloadDecodeError {
    #[must_use]
    pub fn is_too_large(&self) -> bool {
        matches!(self, Self::TooLarge { .. })
    }
}

pub fn decode_command(payload: Option<&[u8]>) -> Result<Command, PayloadDecodeError> {
    decode(payload, "Spaces command")
}

pub fn decode_desired_state(payload: Option<&[u8]>) -> Result<DesiredState, PayloadDecodeError> {
    match payload {
        Some(payload) => decode(Some(payload), "Spaces desired state"),
        None => Ok(DesiredState::default()),
    }
}

pub fn decode_participant_spec(
    payload: Option<&[u8]>,
) -> Result<ParticipantSpec, PayloadDecodeError> {
    decode(payload, "participant profile spec")
}

#[must_use]
pub fn encode_event(event: event::Event) -> Vec<u8> {
    Event { event: Some(event) }.encode_to_vec()
}

#[must_use]
pub fn encode_participant_status(status: ParticipantStatus) -> Vec<u8> {
    status.encode_to_vec()
}

fn decode<M>(payload: Option<&[u8]>, label: &'static str) -> Result<M, PayloadDecodeError>
where
    M: Message + Default,
{
    let bytes = payload.ok_or(PayloadDecodeError::Missing { label })?;
    if bytes.len() > MAX_PROFILE_PAYLOAD_BYTES {
        return Err(PayloadDecodeError::TooLarge {
            label,
            limit: MAX_PROFILE_PAYLOAD_BYTES,
        });
    }
    M::decode(bytes).map_err(|source| PayloadDecodeError::Invalid { label, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_oversized_profile_payloads_fail_closed() {
        assert!(matches!(
            decode_command(None),
            Err(PayloadDecodeError::Missing { .. })
        ));
        assert!(matches!(
            decode_command(Some(&vec![0; MAX_PROFILE_PAYLOAD_BYTES + 1])),
            Err(PayloadDecodeError::TooLarge { .. })
        ));
    }

    #[test]
    fn an_absent_desired_state_is_the_empty_spaces_state() {
        let Ok(state) = decode_desired_state(None) else {
            panic!("absent state must decode as empty");
        };
        assert!(state.observed_space_keys.is_empty());
    }

    #[test]
    fn events_are_encoded_with_the_spaces_envelope() {
        let bytes = encode_event(event::Event::ObservedSpacesAccepted(
            crate::protocol::ObservedSpacesAccepted {
                observed_spaces_revision: 7,
            },
        ));
        let Ok(event) = Event::decode(bytes.as_slice()) else {
            panic!("encoded event must decode");
        };
        assert!(matches!(
            event.event,
            Some(event::Event::ObservedSpacesAccepted(message))
                if message.observed_spaces_revision == 7
        ));
    }
}
