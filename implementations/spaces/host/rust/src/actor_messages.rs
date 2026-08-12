//! Internal actor messages at the composition boundary between Core, Host, and Spaces.

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ClientFrame {
    pub(crate) request_id: Vec<u8>,
    pub(crate) payload: Option<client_frame::Payload>,
}

pub(crate) mod client_frame {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    pub(crate) enum Payload {
        OpenSession(OpenSession),
        RenewLease(RenewLease),
        SyncDesiredState(SyncDesiredState),
        RegisterParticipant(RegisterParticipant),
        SetParticipantSpec(SetParticipantSpec),
        ReleaseParticipant(ReleaseParticipant),
        ReplaceObservedSpaces(ReplaceObservedSpaces),
        FetchSpace(FetchSpace),
        CloseSession(CloseSession),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ServerFrame {
    pub(crate) request_id: Vec<u8>,
    pub(crate) payload: Option<server_frame::Payload>,
}

pub(crate) mod server_frame {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    pub(crate) enum Payload {
        SessionReady(SessionReady),
        DesiredStateReconciled(DesiredStateReconciled),
        ParticipantOwnershipGranted(ParticipantOwnershipGranted),
        ParticipantOwnershipRevoked(ParticipantOwnershipRevoked),
        ParticipantSpecAccepted(ParticipantSpecAccepted),
        ParticipantStatusChanged(ParticipantStatusChanged),
        ObservedSpacesAccepted(ObservedSpacesAccepted),
        SpaceSnapshot(SpaceSnapshot),
        SpaceClosed(SpaceClosed),
        FetchSpaceResult(FetchSpaceResult),
        ResyncRequired(ResyncRequired),
        CommandRejected(CommandRejected),
        SessionClosing(SessionClosing),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpenSession {
    pub(crate) controller_id: String,
    pub(crate) controller_instance_id: Vec<u8>,
    pub(crate) resume_token: Vec<u8>,
    pub(crate) desired_state: Option<DesiredStateSnapshot>,
    pub(crate) profile: Option<ProfileRef>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProfileRef {
    pub(crate) profile_id: String,
    pub(crate) schema_version: u32,
    pub(crate) descriptor_digest: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RenewLease {
    pub(crate) session_token: Vec<u8>,
    pub(crate) desired_state_revision: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SyncDesiredState {
    pub(crate) session_token: Vec<u8>,
    pub(crate) desired_state: Option<DesiredStateSnapshot>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DesiredStateSnapshot {
    pub(crate) desired_state_revision: u64,
    pub(crate) participants: Vec<ParticipantRegistration>,
    pub(crate) observed_space_keys: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParticipantRegistration {
    pub(crate) participant_id: String,
    pub(crate) registration_id: Vec<u8>,
    pub(crate) ownership_token: Vec<u8>,
    pub(crate) client_spec_revision: u64,
    pub(crate) spec: Option<ParticipantSpec>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RegisterParticipant {
    pub(crate) session_token: Vec<u8>,
    pub(crate) participant: Option<ParticipantRegistration>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SetParticipantSpec {
    pub(crate) session_token: Vec<u8>,
    pub(crate) participant_id: String,
    pub(crate) ownership_token: Vec<u8>,
    pub(crate) client_spec_revision: u64,
    pub(crate) spec: Option<ParticipantSpec>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReleaseParticipant {
    pub(crate) session_token: Vec<u8>,
    pub(crate) participant_id: String,
    pub(crate) registration_id: Vec<u8>,
    pub(crate) ownership_token: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReplaceObservedSpaces {
    pub(crate) session_token: Vec<u8>,
    pub(crate) observed_spaces_revision: u64,
    pub(crate) space_keys: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FetchSpace {
    pub(crate) session_token: Vec<u8>,
    pub(crate) space_key: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CloseSession {
    pub(crate) session_token: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParticipantSpec {
    pub(crate) space_key: String,
    pub(crate) display_name: String,
    pub(crate) server_mute: bool,
    pub(crate) server_deaf: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SessionReady {
    pub(crate) session_token: Vec<u8>,
    pub(crate) resume_token: Vec<u8>,
    pub(crate) control_epoch: Vec<u8>,
    pub(crate) lease_duration: Option<prost_types::Duration>,
    pub(crate) profile: Option<ProfileRef>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DesiredStateReconciled {
    pub(crate) desired_state_revision: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParticipantOwnershipGranted {
    pub(crate) participant_id: String,
    pub(crate) registration_id: Vec<u8>,
    pub(crate) ownership_token: Vec<u8>,
    pub(crate) client_spec_revision: u64,
    pub(crate) accepted_spec_revision: u64,
    pub(crate) applied_spec_revision: u64,
    pub(crate) published_generation: u64,
    pub(crate) connection_credential: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub(crate) enum OwnershipRevocationReason {
    OwnershipReplaced = 1,
    SessionExpired = 2,
    ParticipantReleased = 3,
    RegistrationRejected = 4,
}

impl From<OwnershipRevocationReason> for i32 {
    fn from(value: OwnershipRevocationReason) -> Self {
        value as Self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParticipantOwnershipRevoked {
    pub(crate) participant_id: String,
    pub(crate) registration_id: Vec<u8>,
    pub(crate) reason: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParticipantSpecAccepted {
    pub(crate) participant_id: String,
    pub(crate) client_spec_revision: u64,
    pub(crate) accepted_spec_revision: u64,
    pub(crate) applied_spec_revision: u64,
    pub(crate) published_generation: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParticipantStatus {
    pub(crate) connected: bool,
    pub(crate) applied_space_key: String,
    pub(crate) self_mute: bool,
    pub(crate) self_deaf: bool,
    pub(crate) accepted_spec_revision: u64,
    pub(crate) applied_spec_revision: u64,
    pub(crate) published_generation: u64,
    pub(crate) application_error: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParticipantStatusChanged {
    pub(crate) participant_id: String,
    pub(crate) status: Option<ParticipantStatus>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ObservedSpacesAccepted {
    pub(crate) observed_spaces_revision: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpaceParticipant {
    pub(crate) participant_id: String,
    pub(crate) display_name: String,
    pub(crate) server_mute: bool,
    pub(crate) server_deaf: bool,
    pub(crate) connected: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpaceSnapshot {
    pub(crate) space_key: String,
    pub(crate) incarnation_id: Vec<u8>,
    pub(crate) space_revision: u64,
    pub(crate) participants: Vec<SpaceParticipant>,
    pub(crate) published_generation: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpaceClosed {
    pub(crate) space_key: String,
    pub(crate) incarnation_id: Vec<u8>,
    pub(crate) final_space_revision: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FetchSpaceResult {
    pub(crate) result: Option<fetch_space_result::Result>,
}

pub(crate) mod fetch_space_result {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    pub(crate) enum Result {
        Snapshot(SpaceSnapshot),
        Absent(SpaceAbsent),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpaceAbsent {
    pub(crate) space_key: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResyncRequired {
    pub(crate) reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub(crate) enum CommandErrorCode {
    InvalidArgument = 1,
    NotFound = 2,
    OwnershipLost = 3,
    StaleRevision = 4,
    SessionExpiredError = 5,
    ResourceExhausted = 6,
    InternalError = 7,
}

impl From<CommandErrorCode> for i32 {
    fn from(value: CommandErrorCode) -> Self {
        value as Self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CommandRejected {
    pub(crate) code: i32,
    pub(crate) message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SessionClosing {
    pub(crate) reason: String,
}
