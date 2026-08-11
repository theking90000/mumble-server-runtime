use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use mumble_controller_core::{
    ClaimMode, ClaimOutcome, ClaimRequest, OpenSession as CoreOpenSession, OwnershipError,
    OwnershipRegistry, ReliableCommands, RenewOutcome, RevisionOutcome, SessionCredentials,
    SessionError, SessionId, SessionRegistry,
};
use mumble_controller_host::{HostError, MumbleHost, snapshot_channel};
use mumble_controller_spaces::{
    ControllerSpaceLogic, MaterializedSpace, ParticipantSpec as SpaceParticipantSpec,
    ParticipantState as Participant, RenderParticipant, RenderState,
    SessionState as SpacesSessionState, SpaceEvent, SpaceEventKind, SpaceKey, SpaceReport,
    SpaceReporter, SpacesValidationError,
};
use mumble_server_runtime_gateway::RuntimeHandle;
use mumble_server_runtime_shard::{ConnectionId, ReconcileReport, ShardId};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tonic::Status;

use crate::actor_messages::client_frame::Payload as ClientPayload;
use crate::actor_messages::fetch_space_result::Result as FetchResult;
use crate::actor_messages::{
    ClientFrame, CommandErrorCode, CommandRejected, DesiredStateReconciled, DesiredStateSnapshot,
    FetchSpaceResult, ObservedSpacesAccepted, OwnershipRevocationReason,
    ParticipantOwnershipGranted, ParticipantOwnershipRevoked, ParticipantRegistration,
    ParticipantSpec as ProtocolParticipantSpec, ParticipantSpecAccepted, ParticipantStatus,
    ParticipantStatusChanged, ResyncRequired, ServerFrame, SessionClosing, SessionReady,
    SpaceAbsent, SpaceClosed, SpaceParticipant, SpaceSnapshot,
};
use crate::config::ControllerConfig;
use crate::profile;

pub(crate) type ResponseSender = mpsc::Sender<Result<ServerFrame, Status>>;

pub(crate) enum ActorCommand {
    Open {
        stream_id: u64,
        responses: ResponseSender,
        frame: ClientFrame,
    },
    Frame {
        stream_id: u64,
        frame: ClientFrame,
    },
    StreamClosed {
        stream_id: u64,
    },
    Route {
        connection: ConnectionId,
        credential: Option<String>,
        response: oneshot::Sender<Result<ShardId, String>>,
    },
    SpaceEvent(SpaceReport),
    Reconciled {
        space_key: String,
        application_revision: u64,
        report: ReconcileReport,
    },
}

#[derive(Clone)]
struct ActorSpaceReporter {
    sender: mpsc::Sender<ActorCommand>,
}

impl SpaceReporter for ActorSpaceReporter {
    fn try_report(&self, report: SpaceReport) -> Result<(), SpaceReport> {
        let retained = report.clone();
        self.sender
            .try_send(ActorCommand::SpaceEvent(report))
            .map_err(|_error| retained)
    }
}

#[derive(Clone)]
pub(crate) struct ActorHandle {
    sender: mpsc::Sender<ActorCommand>,
}

impl ActorHandle {
    pub(crate) fn sender(&self) -> mpsc::Sender<ActorCommand> {
        self.sender.clone()
    }
}

pub(crate) fn spawn(
    config: ControllerConfig,
    runtime: RuntimeHandle,
) -> Result<(ActorHandle, JoinHandle<()>), ActorStartError> {
    let control_epoch = random_bytes(16)?;
    let (sender, receiver) = mpsc::channel(config.queue_capacity);
    let core_sessions = SessionRegistry::new(config.max_sessions, config.lease_duration);
    let ownership =
        OwnershipRegistry::new(config.max_participants, config.max_participants_per_session);
    let reliable = ReliableCommands::new(config.queue_capacity);
    let actor = ControllerActor {
        config,
        host: MumbleHost::new(runtime),
        sender: sender.clone(),
        control_epoch,
        core_sessions,
        ownership,
        reliable,
        sessions: HashMap::new(),
        streams: HashMap::new(),
        participants: BTreeMap::new(),
        spaces: BTreeMap::new(),
        next_application_revision: 1,
    };
    let task = tokio::spawn(actor.run(receiver));
    Ok((ActorHandle { sender }, task))
}

struct ControllerSession {
    stream_id: Option<u64>,
    responses: Option<ResponseSender>,
    spaces: SpacesSessionState,
}

struct ControllerActor {
    config: ControllerConfig,
    host: MumbleHost,
    sender: mpsc::Sender<ActorCommand>,
    control_epoch: Vec<u8>,
    core_sessions: SessionRegistry,
    ownership: OwnershipRegistry,
    reliable: ReliableCommands<ServerFrame>,
    sessions: HashMap<SessionId, ControllerSession>,
    streams: HashMap<u64, SessionId>,
    participants: BTreeMap<String, Participant>,
    spaces: BTreeMap<String, MaterializedSpace>,
    next_application_revision: u64,
}

impl ControllerActor {
    async fn run(mut self, mut receiver: mpsc::Receiver<ActorCommand>) {
        loop {
            let deadline = self.next_deadline();
            match deadline {
                Some(deadline) => {
                    tokio::select! {
                        // Cancellation-safe: `recv` removes a command only when it completes.
                        command = receiver.recv() => match command {
                            Some(command) => self.handle(command),
                            None => break,
                        },
                        // Cancellation-safe: dropping a sleep loses no state; deadlines live in the actor.
                        () = tokio::time::sleep_until(deadline) => self.expire_due(),
                    }
                }
                None => match receiver.recv().await {
                    Some(command) => self.handle(command),
                    None => break,
                },
            }
        }
    }

    fn handle(&mut self, command: ActorCommand) {
        match command {
            ActorCommand::Open {
                stream_id,
                responses,
                frame,
            } => self.open(stream_id, responses, frame),
            ActorCommand::Frame { stream_id, frame } => self.frame(stream_id, frame),
            ActorCommand::StreamClosed { stream_id } => self.stream_closed(stream_id),
            ActorCommand::Route {
                connection,
                credential,
                response,
            } => self.route(connection, credential, response),
            ActorCommand::SpaceEvent(report) => {
                self.space_event(&report.space_key, report.event);
            }
            ActorCommand::Reconciled {
                space_key,
                application_revision,
                report,
            } => self.reconciled(&space_key, application_revision, report),
        }
    }

    fn open(&mut self, stream_id: u64, responses: ResponseSender, frame: ClientFrame) {
        let request_id = frame.request_id.clone();
        let Some(ClientPayload::OpenSession(open)) = frame.payload else {
            let _result = responses.try_send(Err(Status::invalid_argument(
                "the first Controller frame must be OpenSession",
            )));
            return;
        };
        if request_id.is_empty()
            || open.controller_id.is_empty()
            || open.controller_instance_id.is_empty()
        {
            self.reject_stream(
                &responses,
                request_id,
                CommandErrorCode::InvalidArgument,
                "OpenSession requires request_id, controller_id and controller_instance_id",
            );
            return;
        }
        let Some(requested_profile) = open.profile.as_ref() else {
            self.reject_stream(
                &responses,
                request_id,
                CommandErrorCode::InvalidArgument,
                "OpenSession requires a negotiated profile",
            );
            return;
        };
        let profile = match profile::negotiate(requested_profile) {
            Ok(profile) => profile,
            Err(error) => {
                self.reject_stream(
                    &responses,
                    request_id,
                    CommandErrorCode::InvalidArgument,
                    &error.to_string(),
                );
                return;
            }
        };
        if let Err(message) = self.validate_snapshot(&open.desired_state) {
            self.reject_stream(
                &responses,
                request_id,
                CommandErrorCode::InvalidArgument,
                &message,
            );
            return;
        }

        let session_token = match random_bytes(32) {
            Ok(token) => token,
            Err(error) => {
                self.reject_stream(
                    &responses,
                    request_id,
                    CommandErrorCode::InternalError,
                    &error.to_string(),
                );
                return;
            }
        };
        let resume_token = match random_bytes(32) {
            Ok(token) => token,
            Err(error) => {
                self.reject_stream(
                    &responses,
                    request_id,
                    CommandErrorCode::InternalError,
                    &error.to_string(),
                );
                return;
            }
        };
        let credentials = match SessionCredentials::new(session_token, resume_token) {
            Ok(credentials) => credentials,
            Err(error) => {
                self.reject_stream(
                    &responses,
                    request_id,
                    CommandErrorCode::InternalError,
                    &error.to_string(),
                );
                return;
            }
        };
        let ready = match self.core_sessions.open(CoreOpenSession {
            controller_id: &open.controller_id,
            controller_instance_id: &open.controller_instance_id,
            resume_token: &open.resume_token,
            profile,
            credentials,
            now: Instant::now().into_std(),
        }) {
            Ok(ready) => ready,
            Err(SessionError::ResourceExhausted) => {
                self.reject_stream(
                    &responses,
                    request_id,
                    CommandErrorCode::ResourceExhausted,
                    "the Controller session limit is reached",
                );
                return;
            }
            Err(error) => {
                self.reject_stream(
                    &responses,
                    request_id,
                    CommandErrorCode::InternalError,
                    &error.to_string(),
                );
                return;
            }
        };
        let session_id = ready.session_id;
        if !ready.resumed {
            self.reliable.attach_session(session_id);
            self.sessions.insert(
                session_id,
                ControllerSession {
                    stream_id: None,
                    responses: None,
                    spaces: SpacesSessionState::default(),
                },
            );
        } else if !self.sessions.contains_key(&session_id) {
            self.reject_stream(
                &responses,
                request_id,
                CommandErrorCode::InternalError,
                "the resumed Controller session has no Host state",
            );
            return;
        }

        self.attach_stream(session_id, stream_id, responses);
        let protocol_ready = SessionReady {
            session_token: ready.session_token,
            resume_token: ready.resume_token,
            control_epoch: self.control_epoch.clone(),
            lease_duration: Some(duration_to_proto(self.config.lease_duration)),
            profile: Some(profile::to_protocol(&ready.profile)),
        };
        self.send_session(
            session_id,
            ServerFrame {
                request_id: request_id.clone(),
                payload: Some(crate::actor_messages::server_frame::Payload::SessionReady(
                    protocol_ready,
                )),
            },
        );
        self.reconcile_snapshot(session_id, open.desired_state, request_id);
    }

    fn attach_stream(&mut self, session_id: SessionId, stream_id: u64, responses: ResponseSender) {
        if let Some(previous) = self
            .sessions
            .get(&session_id)
            .and_then(|session| session.stream_id)
        {
            self.streams.remove(&previous);
        }
        self.streams.insert(stream_id, session_id);
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.stream_id = Some(stream_id);
            session.responses = Some(responses);
            self.core_sessions
                .refresh_lease(session_id, Instant::now().into_std());
        }
    }

    fn frame(&mut self, stream_id: u64, frame: ClientFrame) {
        let request_id = frame.request_id.clone();
        if request_id.is_empty() {
            self.reject_stream_id(
                stream_id,
                request_id,
                CommandErrorCode::InvalidArgument,
                "request_id must not be empty",
            );
            return;
        }
        let Some(payload) = frame.payload else {
            self.reject_stream_id(
                stream_id,
                request_id,
                CommandErrorCode::InvalidArgument,
                "ClientFrame has no payload",
            );
            return;
        };
        if matches!(payload, ClientPayload::OpenSession(_)) {
            self.reject_stream_id(
                stream_id,
                request_id,
                CommandErrorCode::InvalidArgument,
                "OpenSession is only valid as the first frame",
            );
            return;
        }
        let Some(session_id) = self.streams.get(&stream_id).copied() else {
            return;
        };
        if let Some(cached) = self.reliable.replay(session_id, &request_id) {
            self.send_session_uncached(session_id, cached);
            return;
        }

        match payload {
            ClientPayload::RenewLease(command) => {
                let renewed = self.core_sessions.renew(
                    session_id,
                    &command.session_token,
                    command.desired_state_revision,
                    Instant::now().into_std(),
                );
                if matches!(renewed, Err(SessionError::InvalidSessionToken)) {
                    self.reject_session(
                        session_id,
                        request_id,
                        CommandErrorCode::SessionExpiredError,
                        "session token is not current",
                    );
                    return;
                }
                if let Err(error) = renewed {
                    self.reject_session(
                        session_id,
                        request_id,
                        CommandErrorCode::InternalError,
                        &error.to_string(),
                    );
                    return;
                }
                if matches!(renewed, Ok(RenewOutcome::ResyncRequired)) {
                    self.send_session(
                        session_id,
                        ServerFrame {
                            request_id,
                            payload: Some(
                                crate::actor_messages::server_frame::Payload::ResyncRequired(
                                    ResyncRequired {
                                        reason: "the server replica no longer matches this session"
                                            .to_owned(),
                                    },
                                ),
                            ),
                        },
                    );
                }
            }
            ClientPayload::SyncDesiredState(command) => {
                if !self.authorized(session_id, &command.session_token) {
                    self.reject_session(
                        session_id,
                        request_id,
                        CommandErrorCode::SessionExpiredError,
                        "session token is not current",
                    );
                    return;
                }
                if let Err(message) = self.validate_snapshot(&command.desired_state) {
                    self.reject_session(
                        session_id,
                        request_id,
                        CommandErrorCode::InvalidArgument,
                        &message,
                    );
                    return;
                }
                self.reconcile_snapshot(session_id, command.desired_state, request_id);
            }
            ClientPayload::RegisterParticipant(command) => {
                if !self.authorized(session_id, &command.session_token) {
                    self.reject_session(
                        session_id,
                        request_id,
                        CommandErrorCode::SessionExpiredError,
                        "session token is not current",
                    );
                    return;
                }
                let Some(registration) = command.participant else {
                    self.reject_session(
                        session_id,
                        request_id,
                        CommandErrorCode::InvalidArgument,
                        "participant registration is missing",
                    );
                    return;
                };
                if let Err(message) = self.validate_registration(&registration) {
                    self.reject_session(
                        session_id,
                        request_id,
                        CommandErrorCode::InvalidArgument,
                        &message,
                    );
                    return;
                }
                self.advance_desired_revision(session_id);
                self.register_explicit(session_id, registration, request_id);
            }
            ClientPayload::SetParticipantSpec(command) => {
                if !self.authorized(session_id, &command.session_token) {
                    self.reject_session(
                        session_id,
                        request_id,
                        CommandErrorCode::SessionExpiredError,
                        "session token is not current",
                    );
                    return;
                }
                let Some(spec) = command.spec else {
                    self.reject_session(
                        session_id,
                        request_id,
                        CommandErrorCode::InvalidArgument,
                        "participant spec is missing",
                    );
                    return;
                };
                let spec = match decode_space_spec(spec) {
                    Ok(spec) => spec,
                    Err(error) => {
                        self.reject_session(
                            session_id,
                            request_id,
                            CommandErrorCode::InvalidArgument,
                            &error.to_string(),
                        );
                        return;
                    }
                };
                self.advance_desired_revision(session_id);
                self.set_spec(
                    session_id,
                    command.participant_id,
                    command.ownership_token,
                    command.client_spec_revision,
                    spec,
                    request_id,
                );
            }
            ClientPayload::ReleaseParticipant(command) => {
                if !self.authorized(session_id, &command.session_token) {
                    self.reject_session(
                        session_id,
                        request_id,
                        CommandErrorCode::SessionExpiredError,
                        "session token is not current",
                    );
                    return;
                }
                self.advance_desired_revision(session_id);
                self.release(
                    session_id,
                    &command.participant_id,
                    &command.registration_id,
                    &command.ownership_token,
                    OwnershipRevocationReason::ParticipantReleased,
                    Some(request_id),
                );
            }
            ClientPayload::ReplaceObservedSpaces(command) => {
                if !self.authorized(session_id, &command.session_token) {
                    self.reject_session(
                        session_id,
                        request_id,
                        CommandErrorCode::SessionExpiredError,
                        "session token is not current",
                    );
                    return;
                }
                self.advance_desired_revision(session_id);
                self.replace_observations(
                    session_id,
                    command.observed_spaces_revision,
                    command.space_keys,
                    request_id,
                );
            }
            ClientPayload::FetchSpace(command) => {
                if !self.authorized(session_id, &command.session_token) {
                    self.reject_session(
                        session_id,
                        request_id,
                        CommandErrorCode::SessionExpiredError,
                        "session token is not current",
                    );
                    return;
                }
                let result = match self.space_snapshot(&command.space_key) {
                    Some(snapshot) => FetchResult::Snapshot(snapshot),
                    None => FetchResult::Absent(SpaceAbsent {
                        space_key: command.space_key,
                    }),
                };
                self.send_session(
                    session_id,
                    ServerFrame {
                        request_id,
                        payload: Some(
                            crate::actor_messages::server_frame::Payload::FetchSpaceResult(
                                FetchSpaceResult {
                                    result: Some(result),
                                },
                            ),
                        ),
                    },
                );
            }
            ClientPayload::CloseSession(command) => {
                if !self.authorized(session_id, &command.session_token) {
                    self.reject_session(
                        session_id,
                        request_id,
                        CommandErrorCode::SessionExpiredError,
                        "session token is not current",
                    );
                    return;
                }
                self.send_session(
                    session_id,
                    ServerFrame {
                        request_id,
                        payload: Some(
                            crate::actor_messages::server_frame::Payload::SessionClosing(
                                SessionClosing {
                                    reason: "session closed by controller".to_owned(),
                                },
                            ),
                        ),
                    },
                );
                self.remove_session(session_id, OwnershipRevocationReason::ParticipantReleased);
            }
            ClientPayload::OpenSession(_) => {}
        }
    }

    fn authorized(&self, session_id: SessionId, token: &[u8]) -> bool {
        self.core_sessions.authorize(session_id, token)
    }

    fn advance_desired_revision(&mut self, session_id: SessionId) {
        self.core_sessions.advance_desired_revision(session_id);
    }

    fn validate_snapshot(&self, snapshot: &Option<DesiredStateSnapshot>) -> Result<(), String> {
        let Some(snapshot) = snapshot else {
            return Err("desired state snapshot is missing".to_owned());
        };
        if snapshot.participants.len() > self.config.max_participants_per_session {
            return Err("participant limit per session is exceeded".to_owned());
        }
        if snapshot.participants.len() > self.config.max_participants {
            return Err("global participant limit is exceeded".to_owned());
        }
        if snapshot.observed_space_keys.len() > self.config.max_observations_per_session {
            return Err("observation limit per session is exceeded".to_owned());
        }
        let mut participant_ids = BTreeSet::new();
        for registration in &snapshot.participants {
            self.validate_registration(registration)?;
            if !participant_ids.insert(&registration.participant_id) {
                return Err("desired snapshot contains a duplicate participant".to_owned());
            }
        }
        let desired_spaces: BTreeSet<&str> = snapshot
            .participants
            .iter()
            .filter_map(|registration| registration.spec.as_ref())
            .map(|spec| spec.space_key.as_str())
            .collect();
        if desired_spaces.len() > self.config.max_spaces {
            return Err("materialized Space limit is exceeded".to_owned());
        }
        let observations: BTreeSet<&str> = snapshot
            .observed_space_keys
            .iter()
            .map(String::as_str)
            .collect();
        if observations.len() != snapshot.observed_space_keys.len() {
            return Err("desired snapshot contains a duplicate observation".to_owned());
        }
        for key in observations {
            validate_space_key(key)?;
        }
        Ok(())
    }

    fn validate_registration(&self, registration: &ParticipantRegistration) -> Result<(), String> {
        if registration.participant_id.is_empty() || registration.participant_id.len() > 128 {
            return Err("participant_id must contain between 1 and 128 bytes".to_owned());
        }
        if registration.registration_id.is_empty() {
            return Err("registration_id must not be empty".to_owned());
        }
        let Some(spec) = &registration.spec else {
            return Err("participant spec is missing".to_owned());
        };
        decode_space_spec(spec.clone())
            .map(|_spec| ())
            .map_err(|error| error.to_string())
    }

    fn reconcile_snapshot(
        &mut self,
        session_id: SessionId,
        snapshot: Option<DesiredStateSnapshot>,
        request_id: Vec<u8>,
    ) {
        let Some(snapshot) = snapshot else {
            return;
        };
        let desired_ids: BTreeSet<String> = snapshot
            .participants
            .iter()
            .map(|registration| registration.participant_id.clone())
            .collect();
        let no_longer_desired: Vec<String> = self
            .ownership
            .owned_entities(session_id)
            .into_iter()
            .filter(|entity_id| !desired_ids.contains(entity_id))
            .collect();
        for participant_id in no_longer_desired {
            let Some(ownership) = self.ownership.get(&participant_id).cloned() else {
                continue;
            };
            self.release(
                session_id,
                &participant_id,
                &ownership.registration_id,
                &ownership.fencing_token,
                OwnershipRevocationReason::ParticipantReleased,
                None,
            );
        }

        self.core_sessions
            .set_desired_revision(session_id, snapshot.desired_state_revision);
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session
                .spaces
                .replace_snapshot_observations(snapshot.observed_space_keys);
        }
        for registration in snapshot.participants {
            self.register_from_snapshot(session_id, registration, request_id.clone());
        }
        self.send_session(
            session_id,
            ServerFrame {
                request_id,
                payload: Some(
                    crate::actor_messages::server_frame::Payload::DesiredStateReconciled(
                        DesiredStateReconciled {
                            desired_state_revision: snapshot.desired_state_revision,
                        },
                    ),
                ),
            },
        );
        self.send_effective_space_snapshots(session_id);
    }

    fn register_from_snapshot(
        &mut self,
        session_id: SessionId,
        registration: ParticipantRegistration,
        request_id: Vec<u8>,
    ) {
        if let Some(current) = self.ownership.get(&registration.participant_id).cloned() {
            if current.owner == session_id
                && current.registration_id == registration.registration_id
                && current.fencing_token == registration.ownership_token
            {
                self.resume_registration(session_id, registration, request_id, ClaimMode::Snapshot);
                return;
            }
            // A full snapshot restores capabilities but never transfers ownership.
            // An explicit RegisterParticipant is required to replace another owner.
            self.send_revocation(
                session_id,
                &registration.participant_id,
                registration.registration_id,
                OwnershipRevocationReason::RegistrationRejected,
                request_id,
            );
            return;
        }
        self.acquire(session_id, registration, request_id, ClaimMode::Snapshot);
    }

    fn register_explicit(
        &mut self,
        session_id: SessionId,
        registration: ParticipantRegistration,
        request_id: Vec<u8>,
    ) {
        if let Some(current) = self.ownership.get(&registration.participant_id)
            && current.owner == session_id
            && current.registration_id == registration.registration_id
        {
            self.resume_registration(session_id, registration, request_id, ClaimMode::Explicit);
            return;
        }
        self.acquire(session_id, registration, request_id, ClaimMode::Explicit);
    }

    fn resume_registration(
        &mut self,
        session_id: SessionId,
        registration: ParticipantRegistration,
        request_id: Vec<u8>,
        mode: ClaimMode,
    ) {
        let participant_id = registration.participant_id.clone();
        let spec = match registration.spec.clone().map(decode_space_spec) {
            Some(Ok(spec)) => spec,
            Some(Err(error)) => {
                self.reject_session(
                    session_id,
                    request_id,
                    CommandErrorCode::InvalidArgument,
                    &error.to_string(),
                );
                return;
            }
            None => {
                self.reject_session(
                    session_id,
                    request_id,
                    CommandErrorCode::InvalidArgument,
                    "participant spec is missing",
                );
                return;
            }
        };
        if !self.participants.contains_key(&participant_id) {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::InternalError,
                "resumed ownership has no Host participant state",
            );
            return;
        }
        let claim = self.ownership.claim(ClaimRequest {
            entity_id: &participant_id,
            owner: session_id,
            registration_id: &registration.registration_id,
            presented_fencing_token: &registration.ownership_token,
            client_revision: registration.client_spec_revision,
            new_fencing_token: Vec::new(),
            mode,
        });
        let revision_advanced = match claim {
            Ok(ClaimOutcome::Resumed {
                revision_advanced, ..
            }) => revision_advanced,
            Ok(ClaimOutcome::Acquired { .. }) => {
                self.reject_session(
                    session_id,
                    request_id,
                    CommandErrorCode::InternalError,
                    "a resumed registration unexpectedly acquired ownership",
                );
                return;
            }
            Err(error) => {
                self.reject_session(
                    session_id,
                    request_id,
                    CommandErrorCode::InternalError,
                    &error.to_string(),
                );
                return;
            }
        };
        if revision_advanced {
            self.apply_spec(&participant_id, spec);
        }
        if let Some(participant) = self.participants.get(&participant_id).cloned() {
            self.send_grant(session_id, &participant, request_id);
        }
    }

    fn acquire(
        &mut self,
        session_id: SessionId,
        registration: ParticipantRegistration,
        request_id: Vec<u8>,
        mode: ClaimMode,
    ) {
        let spec = match registration.spec.clone().map(decode_space_spec) {
            Some(Ok(spec)) => spec,
            Some(Err(error)) => {
                self.reject_session(
                    session_id,
                    request_id,
                    CommandErrorCode::InvalidArgument,
                    &error.to_string(),
                );
                return;
            }
            None => {
                self.reject_session(
                    session_id,
                    request_id,
                    CommandErrorCode::InvalidArgument,
                    "participant spec is missing",
                );
                return;
            }
        };
        let existing = self.ownership.get(&registration.participant_id).cloned();
        if existing.is_some() != self.participants.contains_key(&registration.participant_id)
            || existing.is_some() != self.host.contains(&registration.participant_id)
        {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::InternalError,
                "Core ownership and Host participant state disagree",
            );
            return;
        }
        if existing.is_none() && self.participants.len() >= self.config.max_participants {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::ResourceExhausted,
                "the global participant limit is reached",
            );
            return;
        }
        let owned = self.ownership.owned_entities(session_id).len();
        if existing
            .as_ref()
            .is_none_or(|ownership| ownership.owner != session_id)
            && owned >= self.config.max_participants_per_session
        {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::ResourceExhausted,
                "the participant limit per session is reached",
            );
            return;
        }
        if let Err(error) = self.ensure_space(spec.space_key().as_str()) {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::ResourceExhausted,
                &error,
            );
            return;
        }
        let ownership_token = match random_bytes(32) {
            Ok(token) => token,
            Err(error) => {
                self.reject_session(
                    session_id,
                    request_id,
                    CommandErrorCode::InternalError,
                    &error.to_string(),
                );
                return;
            }
        };
        let join_token = match random_join_token() {
            Ok(token) => token,
            Err(error) => {
                self.reject_session(
                    session_id,
                    request_id,
                    CommandErrorCode::InternalError,
                    &error.to_string(),
                );
                return;
            }
        };
        if !self.host.credential_available(&join_token) {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::InternalError,
                "generated Mumble credential collides with a live registration",
            );
            return;
        }
        let mut self_mute = false;
        let mut self_deaf = false;
        let mut applied_space_key = None;
        let mut published_generation = 0;
        let claim = self.ownership.claim(ClaimRequest {
            entity_id: &registration.participant_id,
            owner: session_id,
            registration_id: &registration.registration_id,
            presented_fencing_token: &registration.ownership_token,
            client_revision: registration.client_spec_revision,
            new_fencing_token: ownership_token,
            mode,
        });
        let previous_ownership = match claim {
            Ok(ClaimOutcome::Acquired { previous, .. }) => previous,
            Ok(ClaimOutcome::Resumed { .. }) => {
                self.reject_session(
                    session_id,
                    request_id,
                    CommandErrorCode::InternalError,
                    "an ownership acquisition unexpectedly resumed a registration",
                );
                return;
            }
            Err(OwnershipError::GlobalLimit | OwnershipError::OwnerLimit) => {
                self.reject_session(
                    session_id,
                    request_id,
                    CommandErrorCode::ResourceExhausted,
                    "the ownership limit is reached",
                );
                return;
            }
            Err(OwnershipError::SnapshotConflict) => {
                self.send_revocation(
                    session_id,
                    &registration.participant_id,
                    registration.registration_id,
                    OwnershipRevocationReason::RegistrationRejected,
                    request_id,
                );
                return;
            }
            Err(error) => {
                self.reject_session(
                    session_id,
                    request_id,
                    CommandErrorCode::InternalError,
                    &error.to_string(),
                );
                return;
            }
        };
        if let Some(previous_ownership) = previous_ownership {
            let Some(previous) = self.participants.get(&registration.participant_id).cloned()
            else {
                self.reject_session(
                    session_id,
                    request_id,
                    CommandErrorCode::InternalError,
                    "replaced ownership has no Host participant state",
                );
                return;
            };
            self_mute = previous.self_mute;
            self_deaf = previous.self_deaf;
            applied_space_key = previous.applied_space_key.clone();
            published_generation = previous.published_generation;
            self.advance_desired_revision(previous_ownership.owner);
            self.send_revocation(
                previous_ownership.owner,
                &previous.participant_id,
                previous_ownership.registration_id,
                OwnershipRevocationReason::OwnershipReplaced,
                Vec::new(),
            );
        }
        if let Err(error) = self.host.register(&registration.participant_id, join_token) {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::InternalError,
                &error.to_string(),
            );
            return;
        }
        let participant = Participant {
            participant_id: registration.participant_id.clone(),
            // Client revisions belong to one registration capability. A transfer
            // keeps the live connection, not the previous owner's revision space.
            applied_spec_revision: 0,
            applied_space_key,
            published_generation,
            spec,
            self_mute,
            self_deaf,
            application_error: String::new(),
        };
        let old_space = self
            .participants
            .insert(participant.participant_id.clone(), participant.clone())
            .map(|previous| previous.spec.space_key().as_str().to_owned());
        if let Some(old_space) = old_space
            && old_space != participant.spec.space_key().as_str()
        {
            self.refresh_space(&old_space);
            self.refresh_space(participant.spec.space_key().as_str());
            if let Some(connection) = self.host.connection(&participant.participant_id)
                && let Some(space) = self.spaces.get(participant.spec.space_key().as_str())
            {
                self.host
                    .move_connection(connection, space.shard_handle().shard());
            }
        } else {
            self.refresh_space(participant.spec.space_key().as_str());
        }
        self.send_grant(session_id, &participant, request_id);
        self.send_status(&participant.participant_id);
    }

    fn set_spec(
        &mut self,
        session_id: SessionId,
        participant_id: String,
        ownership_token: Vec<u8>,
        client_revision: u64,
        spec: SpaceParticipantSpec,
        request_id: Vec<u8>,
    ) {
        let Some(current) = self.participants.get(&participant_id).cloned() else {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::NotFound,
                "participant is not registered",
            );
            return;
        };
        let Some(current_ownership) = self.ownership.get(&participant_id).cloned() else {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::InternalError,
                "participant Host state has no ownership state",
            );
            return;
        };
        if current_ownership.owner != session_id
            || current_ownership.fencing_token != ownership_token
        {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::OwnershipLost,
                "participant ownership token is stale",
            );
            return;
        }
        if client_revision < current_ownership.client_revision {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::StaleRevision,
                "participant spec revision moved backwards",
            );
            return;
        }
        if client_revision == current_ownership.client_revision && spec != current.spec {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::StaleRevision,
                "a participant spec cannot change without advancing its revision",
            );
            return;
        }
        if let Err(error) = self.ensure_space(spec.space_key().as_str()) {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::ResourceExhausted,
                &error,
            );
            return;
        }
        let revision = self.ownership.accept_revision(
            &participant_id,
            session_id,
            &ownership_token,
            client_revision,
            spec == current.spec,
        );
        match revision {
            Ok(RevisionOutcome::Advanced) => {
                self.apply_spec(&participant_id, spec);
            }
            Ok(RevisionOutcome::Unchanged) => {}
            Err(OwnershipError::NotFound) => {
                self.reject_session(
                    session_id,
                    request_id,
                    CommandErrorCode::NotFound,
                    "participant is not registered",
                );
                return;
            }
            Err(OwnershipError::OwnershipLost) => {
                self.reject_session(
                    session_id,
                    request_id,
                    CommandErrorCode::OwnershipLost,
                    "participant ownership token is stale",
                );
                return;
            }
            Err(OwnershipError::StaleRevision) => {
                self.reject_session(
                    session_id,
                    request_id,
                    CommandErrorCode::StaleRevision,
                    "participant spec revision is stale",
                );
                return;
            }
            Err(error) => {
                self.reject_session(
                    session_id,
                    request_id,
                    CommandErrorCode::InternalError,
                    &error.to_string(),
                );
                return;
            }
        }
        let Some(participant) = self.participants.get(&participant_id).cloned() else {
            return;
        };
        let Some(ownership) = self.ownership.get(&participant_id) else {
            return;
        };
        self.send_session(
            session_id,
            ServerFrame {
                request_id,
                payload: Some(
                    crate::actor_messages::server_frame::Payload::ParticipantSpecAccepted(
                        ParticipantSpecAccepted {
                            participant_id,
                            client_spec_revision: ownership.client_revision,
                            accepted_spec_revision: ownership.client_revision,
                            applied_spec_revision: participant.applied_spec_revision,
                            published_generation: participant.published_generation,
                        },
                    ),
                ),
            },
        );
    }

    fn apply_spec(&mut self, participant_id: &str, spec: SpaceParticipantSpec) {
        let Some(current) = self.participants.get(participant_id).cloned() else {
            return;
        };
        let old_space = current.spec.space_key().as_str().to_owned();
        let new_space = spec.space_key().as_str().to_owned();
        let connection = self.host.connection(participant_id);
        if let Some(participant) = self.participants.get_mut(participant_id) {
            participant.spec = spec;
        }
        self.refresh_space(&old_space);
        if new_space != old_space {
            self.refresh_space(&new_space);
            if let Some(connection) = connection
                && let Some(space) = self.spaces.get(&new_space)
            {
                self.host
                    .move_connection(connection, space.shard_handle().shard());
            }
        }
    }

    fn release(
        &mut self,
        session_id: SessionId,
        participant_id: &str,
        registration_id: &[u8],
        ownership_token: &[u8],
        reason: OwnershipRevocationReason,
        request_id: Option<Vec<u8>>,
    ) {
        if !self.participants.contains_key(participant_id) {
            if let Some(request_id) = request_id {
                let (code, message) = if self.ownership.get(participant_id).is_some() {
                    (
                        CommandErrorCode::InternalError,
                        "ownership has no Host participant state",
                    )
                } else {
                    (
                        CommandErrorCode::OwnershipLost,
                        "participant is no longer owned",
                    )
                };
                self.reject_session(session_id, request_id, code, message);
            }
            return;
        }
        let released =
            self.ownership
                .release(participant_id, session_id, registration_id, ownership_token);
        let released = match released {
            Ok(released) => released,
            Err(OwnershipError::OwnershipLost | OwnershipError::NotFound) => {
                if let Some(request_id) = request_id {
                    self.reject_session(
                        session_id,
                        request_id,
                        CommandErrorCode::OwnershipLost,
                        "participant ownership token is stale",
                    );
                }
                return;
            }
            Err(error) => {
                if let Some(request_id) = request_id {
                    self.reject_session(
                        session_id,
                        request_id,
                        CommandErrorCode::InternalError,
                        &error.to_string(),
                    );
                }
                return;
            }
        };
        let Some(current) = self.participants.remove(participant_id) else {
            return;
        };
        self.host.revoke(participant_id);
        self.refresh_space(current.spec.space_key().as_str());
        self.send_revocation(
            session_id,
            participant_id,
            released.registration_id,
            reason,
            request_id.unwrap_or_default(),
        );
    }

    fn replace_observations(
        &mut self,
        session_id: SessionId,
        revision: u64,
        keys: Vec<String>,
        request_id: Vec<u8>,
    ) {
        if keys.len() > self.config.max_observations_per_session
            || keys.iter().any(|key| validate_space_key(key).is_err())
        {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::InvalidArgument,
                "invalid explicit observation set",
            );
            return;
        }
        let key_count = keys.len();
        let set: BTreeSet<String> = keys.into_iter().collect();
        if set.len() != key_count {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::InvalidArgument,
                "explicit observation set contains a duplicate Space",
            );
            return;
        }
        let stale = self
            .sessions
            .get_mut(&session_id)
            .is_some_and(|session| session.spaces.replace_observations(revision, set).is_err());
        if stale {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::StaleRevision,
                "observation revision moved backwards",
            );
            return;
        }
        self.send_session(
            session_id,
            ServerFrame {
                request_id,
                payload: Some(
                    crate::actor_messages::server_frame::Payload::ObservedSpacesAccepted(
                        ObservedSpacesAccepted {
                            observed_spaces_revision: revision,
                        },
                    ),
                ),
            },
        );
        self.send_effective_space_snapshots(session_id);
    }

    fn ensure_space(&mut self, space_key: &str) -> Result<(), String> {
        if self.spaces.contains_key(space_key) {
            return Ok(());
        }
        if self.spaces.len() >= self.config.max_spaces {
            return Err("the materialized Space limit is reached".to_owned());
        }
        let incarnation_id = random_bytes(16).map_err(|error| error.to_string())?;
        let initial = Arc::new(RenderState {
            application_revision: 0,
            space_key: space_key.to_owned(),
            participants: Vec::new(),
        });
        let (desired, receiver) = snapshot_channel(initial);
        let publication_marker = desired.publication_marker();
        let actor_for_logic = self.sender.clone();
        let actor_for_report = self.sender.clone();
        let key_for_logic = space_key.to_owned();
        let key_for_report = space_key.to_owned();
        let (report_sender, mut report_receiver) = watch::channel(None::<(u64, ReconcileReport)>);
        let handle = self.host.create_shard_with_reports(
            move |_handle| {
                ControllerSpaceLogic::new(
                    key_for_logic,
                    receiver,
                    ActorSpaceReporter {
                        sender: actor_for_logic,
                    },
                )
            },
            move |report| {
                report_sender.send_replace(Some((
                    publication_marker.rendered_revision(),
                    report.clone(),
                )));
            },
        );
        // A watch channel keeps the newest complete result when renders coalesce.
        // The bridge is the only asynchronous waiter and applies bounded actor
        // backpressure without ever blocking the shard task.
        tokio::spawn(async move {
            while report_receiver.changed().await.is_ok() {
                let latest = report_receiver.borrow_and_update().clone();
                let Some((application_revision, report)) = latest else {
                    continue;
                };
                if actor_for_report
                    .send(ActorCommand::Reconciled {
                        space_key: key_for_report.clone(),
                        application_revision,
                        report,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        self.spaces.insert(
            space_key.to_owned(),
            MaterializedSpace::new(handle, incarnation_id, desired),
        );
        Ok(())
    }

    fn refresh_space(&mut self, space_key: &str) {
        let participants: Vec<RenderParticipant> = self
            .participants
            .values()
            .filter(|participant| participant.spec.space_key().as_str() == space_key)
            .filter_map(|participant| {
                let ownership = self.ownership.get(&participant.participant_id)?;
                Some(RenderParticipant {
                    participant_id: participant.participant_id.clone(),
                    connection: self.host.connection(&participant.participant_id),
                    display_name: participant.spec.display_name().to_owned(),
                    server_mute: participant.spec.server_mute(),
                    server_deaf: participant.spec.server_deaf(),
                    self_mute: participant.self_mute,
                    self_deaf: participant.self_deaf,
                    accepted_revision: ownership.client_revision,
                })
            })
            .collect();
        let application_revision = self.next_application_revision;
        // Saturation is an explicit terminal watermark: it preserves ordering instead of wrapping.
        self.next_application_revision = self.next_application_revision.saturating_add(1);
        let Some(space) = self.spaces.get_mut(space_key) else {
            return;
        };
        space.refresh(
            application_revision,
            space_key,
            participants,
            Instant::now().into_std(),
            self.config.empty_space_grace,
        );
        self.broadcast_space_snapshot(space_key);
    }

    fn reconciled(&mut self, space_key: &str, application_revision: u64, report: ReconcileReport) {
        let Some(space) = self.spaces.get_mut(space_key) else {
            return;
        };
        let Some(reconciliation) = space.reconcile(application_revision, &report) else {
            return;
        };
        let participant_ids: Vec<String> = reconciliation.revisions.keys().cloned().collect();
        for (participant_id, applied_revision) in reconciliation.revisions {
            if let Some(participant) = self.participants.get_mut(&participant_id) {
                // Each Space reports through its own bridge task, so a report rendered
                // before a move can reach the actor after the destination Space has
                // already reported. Applying it would publish a Space the participant
                // has left, and nothing would correct it until the next render there.
                if participant.spec.space_key().as_str() != space_key {
                    continue;
                }
                match &reconciliation.refusal {
                    Some(error) => participant.application_error = error.clone(),
                    None => {
                        participant.applied_spec_revision =
                            participant.applied_spec_revision.max(applied_revision);
                        participant.applied_space_key = Some(space_key.to_owned());
                        participant.published_generation = reconciliation.published_generation;
                        participant.application_error.clear();
                    }
                }
            }
        }
        for participant_id in participant_ids {
            self.send_status(&participant_id);
        }
        self.broadcast_space_snapshot(space_key);
    }

    fn route(
        &mut self,
        connection: ConnectionId,
        credential: Option<String>,
        response: oneshot::Sender<Result<ShardId, String>>,
    ) {
        let Some(credential) = credential else {
            let _ignored = response.send(Err("a Mumble join token is required".to_owned()));
            return;
        };
        let attachment = match self.host.attach(&credential, connection) {
            Ok(attachment) => attachment,
            Err(HostError::UnknownCredential) => {
                let _ignored =
                    response.send(Err("the Mumble join token is invalid or revoked".to_owned()));
                return;
            }
            Err(error) => {
                let _ignored = response.send(Err(error.to_string()));
                return;
            }
        };
        let participant_id = attachment.participant_id;
        let Some(current) = self.participants.get(&participant_id).cloned() else {
            let _ignored =
                response.send(Err("the Mumble join token is invalid or revoked".to_owned()));
            return;
        };
        self.refresh_space(current.spec.space_key().as_str());
        self.send_status(&participant_id);
        let result = self
            .spaces
            .get(current.spec.space_key().as_str())
            .map(|space| space.shard_handle().shard())
            .ok_or_else(|| "the participant Space is not materialized".to_owned());
        let _ignored = response.send(result);
    }

    fn space_event(&mut self, space_key: &str, event: SpaceEvent) {
        let connection = event.connection;
        let participant_id = self.host.participant(connection).map(str::to_owned);
        let Some(participant_id) = participant_id else {
            return;
        };
        let mut refresh = false;
        if let Some(participant) = self.participants.get_mut(&participant_id) {
            if participant.spec.space_key().as_str() != space_key {
                return;
            }
            match event.kind {
                SpaceEventKind::Connected => {}
                SpaceEventKind::Disconnected => {
                    self.host.disconnect(connection);
                    refresh = true;
                }
                SpaceEventKind::SelfState {
                    self_mute,
                    self_deaf,
                } => {
                    participant.self_mute = self_mute;
                    participant.self_deaf = self_deaf;
                    refresh = true;
                }
            }
        }
        if refresh {
            self.refresh_space(space_key);
        }
        self.send_status(&participant_id);
    }

    fn stream_closed(&mut self, stream_id: u64) {
        let Some(session_id) = self.streams.remove(&stream_id) else {
            return;
        };
        if let Some(session) = self.sessions.get_mut(&session_id)
            && session.stream_id == Some(stream_id)
        {
            session.stream_id = None;
            session.responses = None;
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.core_sessions
            .next_deadline()
            .map(Instant::from_std)
            .into_iter()
            .chain(
                self.spaces
                    .values()
                    .filter_map(MaterializedSpace::close_deadline)
                    .map(Instant::from_std),
            )
            .min()
    }

    fn expire_due(&mut self) {
        let now = Instant::now();
        let expired_sessions = self.core_sessions.expired_sessions(now.into_std());
        for session_id in expired_sessions {
            self.remove_session(session_id, OwnershipRevocationReason::SessionExpired);
        }
        let closed_spaces: Vec<String> = self
            .spaces
            .iter()
            .filter(|(_, space)| {
                space
                    .close_deadline()
                    .is_some_and(|deadline| deadline <= now.into_std())
            })
            .map(|(space_key, _)| space_key.clone())
            .collect();
        for space_key in closed_spaces {
            if self
                .participants
                .values()
                .any(|participant| participant.spec.space_key().as_str() == space_key)
            {
                continue;
            }
            let Some(space) = self.spaces.remove(&space_key) else {
                continue;
            };
            self.host
                .destroy_shard(space.shard_handle().shard(), "empty Space grace elapsed");
            self.broadcast_space_closed(&space_key, &space);
        }
    }

    fn remove_session(&mut self, session_id: SessionId, reason: OwnershipRevocationReason) {
        let participant_ids = self.ownership.owned_entities(session_id);
        for participant_id in participant_ids {
            let Some(ownership) = self.ownership.get(&participant_id).cloned() else {
                continue;
            };
            self.release(
                session_id,
                &participant_id,
                &ownership.registration_id,
                &ownership.fencing_token,
                reason,
                None,
            );
        }
        self.core_sessions.remove(session_id);
        self.reliable.remove_session(session_id);
        if let Some(session) = self.sessions.remove(&session_id)
            && let Some(stream_id) = session.stream_id
        {
            self.streams.remove(&stream_id);
        }
    }

    fn send_grant(
        &mut self,
        session_id: SessionId,
        participant: &Participant,
        request_id: Vec<u8>,
    ) {
        let Some(ownership) = self.ownership.get(&participant.participant_id).cloned() else {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::InternalError,
                "participant Host state has no ownership state",
            );
            return;
        };
        let Some(join_token) = self
            .host
            .credential(&participant.participant_id)
            .map(str::to_owned)
        else {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::InternalError,
                "participant Host state has no Mumble credential",
            );
            return;
        };
        self.send_session(
            session_id,
            ServerFrame {
                request_id,
                payload: Some(
                    crate::actor_messages::server_frame::Payload::ParticipantOwnershipGranted(
                        ParticipantOwnershipGranted {
                            participant_id: participant.participant_id.clone(),
                            registration_id: ownership.registration_id,
                            ownership_token: ownership.fencing_token,
                            client_spec_revision: ownership.client_revision,
                            accepted_spec_revision: ownership.client_revision,
                            applied_spec_revision: participant.applied_spec_revision,
                            published_generation: participant.published_generation,
                            connection_credential: join_token,
                        },
                    ),
                ),
            },
        );
    }

    fn send_revocation(
        &mut self,
        session_id: SessionId,
        participant_id: &str,
        registration_id: Vec<u8>,
        reason: OwnershipRevocationReason,
        request_id: Vec<u8>,
    ) {
        self.send_session(
            session_id,
            ServerFrame {
                request_id,
                payload: Some(
                    crate::actor_messages::server_frame::Payload::ParticipantOwnershipRevoked(
                        ParticipantOwnershipRevoked {
                            participant_id: participant_id.to_owned(),
                            registration_id,
                            reason: reason.into(),
                        },
                    ),
                ),
            },
        );
    }

    fn send_status(&mut self, participant_id: &str) {
        let Some(participant) = self.participants.get(participant_id).cloned() else {
            return;
        };
        let Some(ownership) = self.ownership.get(participant_id).cloned() else {
            return;
        };
        let connected = self.host.connection(&participant.participant_id).is_some();
        self.send_session(
            ownership.owner,
            ServerFrame {
                request_id: Vec::new(),
                payload: Some(
                    crate::actor_messages::server_frame::Payload::ParticipantStatusChanged(
                        ParticipantStatusChanged {
                            participant_id: participant.participant_id,
                            status: Some(ParticipantStatus {
                                connected,
                                applied_space_key: participant
                                    .applied_space_key
                                    .unwrap_or_default(),
                                self_mute: participant.self_mute,
                                self_deaf: participant.self_deaf,
                                accepted_spec_revision: ownership.client_revision,
                                applied_spec_revision: participant.applied_spec_revision,
                                published_generation: participant.published_generation,
                                application_error: participant.application_error,
                            }),
                        },
                    ),
                ),
            },
        );
    }

    fn space_snapshot(&self, space_key: &str) -> Option<SpaceSnapshot> {
        let space = self.spaces.get(space_key)?;
        let participants = self
            .participants
            .values()
            .filter(|participant| participant.spec.space_key().as_str() == space_key)
            .map(|participant| SpaceParticipant {
                participant_id: participant.participant_id.clone(),
                display_name: participant.spec.display_name().to_owned(),
                server_mute: participant.spec.server_mute(),
                server_deaf: participant.spec.server_deaf(),
                connected: self.host.connection(&participant.participant_id).is_some(),
            })
            .collect();
        Some(SpaceSnapshot {
            space_key: space_key.to_owned(),
            incarnation_id: space.incarnation_id().to_vec(),
            space_revision: space.revision(),
            participants,
            published_generation: space.published_generation(),
        })
    }

    fn send_effective_space_snapshots(&mut self, session_id: SessionId) {
        let keys: Vec<String> = self
            .spaces
            .keys()
            .filter(|space_key| self.session_observes(session_id, space_key))
            .cloned()
            .collect();
        for key in keys {
            if let Some(snapshot) = self.space_snapshot(&key) {
                self.send_session(
                    session_id,
                    ServerFrame {
                        request_id: Vec::new(),
                        payload: Some(crate::actor_messages::server_frame::Payload::SpaceSnapshot(
                            snapshot,
                        )),
                    },
                );
            }
        }
    }

    fn broadcast_space_snapshot(&mut self, space_key: &str) {
        let Some(snapshot) = self.space_snapshot(space_key) else {
            return;
        };
        let sessions: Vec<SessionId> = self
            .sessions
            .keys()
            .filter(|session_id| self.session_observes(**session_id, space_key))
            .copied()
            .collect();
        for session_id in sessions {
            self.send_session(
                session_id,
                ServerFrame {
                    request_id: Vec::new(),
                    payload: Some(crate::actor_messages::server_frame::Payload::SpaceSnapshot(
                        snapshot.clone(),
                    )),
                },
            );
        }
    }

    fn broadcast_space_closed(&mut self, space_key: &str, space: &MaterializedSpace) {
        let sessions: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, session)| session.spaces.observes(space_key))
            .map(|(session_id, _)| *session_id)
            .collect();
        for session_id in sessions {
            self.send_session(
                session_id,
                ServerFrame {
                    request_id: Vec::new(),
                    payload: Some(crate::actor_messages::server_frame::Payload::SpaceClosed(
                        SpaceClosed {
                            space_key: space_key.to_owned(),
                            incarnation_id: space.incarnation_id().to_vec(),
                            final_space_revision: space.revision(),
                        },
                    )),
                },
            );
        }
    }

    fn session_observes(&self, session_id: SessionId, space_key: &str) -> bool {
        self.sessions
            .get(&session_id)
            .is_some_and(|session| session.spaces.observes(space_key))
            || self.participants.values().any(|participant| {
                self.ownership
                    .get(&participant.participant_id)
                    .is_some_and(|ownership| ownership.owner == session_id)
                    && participant.spec.space_key().as_str() == space_key
            })
    }

    fn send_session(&mut self, session_id: SessionId, frame: ServerFrame) {
        if !frame.request_id.is_empty()
            && let Err(error) =
                self.reliable
                    .complete(session_id, frame.request_id.clone(), frame.clone())
        {
            eprintln!("mumble-controller-server: dropping an unrecorded reliable result: {error}");
            self.core_sessions.mark_resync_required(session_id);
            return;
        }
        self.send_session_uncached(session_id, frame);
    }

    fn send_session_uncached(&mut self, session_id: SessionId, frame: ServerFrame) {
        let sender = self
            .sessions
            .get(&session_id)
            .and_then(|session| session.responses.clone());
        let Some(sender) = sender else {
            return;
        };
        match sender.try_send(Ok(frame)) {
            Ok(()) => {}
            // A momentarily full queue is not a dead stream. Detaching it here would
            // silently drop every later command, including RenewLease, and the lease
            // would expire on a controller that never learned anything went wrong.
            Err(mpsc::error::TrySendError::Full(_dropped)) => {
                self.core_sessions.mark_resync_required(session_id);
            }
            Err(mpsc::error::TrySendError::Closed(_dropped)) => {
                if let Some(session) = self.sessions.get_mut(&session_id) {
                    if let Some(stream_id) = session.stream_id.take() {
                        self.streams.remove(&stream_id);
                    }
                    session.responses = None;
                }
            }
        }
    }

    fn reject_session(
        &mut self,
        session_id: SessionId,
        request_id: Vec<u8>,
        code: CommandErrorCode,
        message: &str,
    ) {
        self.send_session(session_id, rejection(request_id, code, message));
    }

    fn reject_stream_id(
        &mut self,
        stream_id: u64,
        request_id: Vec<u8>,
        code: CommandErrorCode,
        message: &str,
    ) {
        if let Some(session_id) = self.streams.get(&stream_id).copied() {
            self.reject_session(session_id, request_id, code, message);
        }
    }

    /// Reject a stream that never became a session, then end it.
    ///
    /// Nothing maps `stream_id` to a session yet, so any later frame on this
    /// stream would be dropped without an answer. Terminating the response
    /// stream is the only outcome the controller can observe.
    fn reject_stream(
        &self,
        responses: &ResponseSender,
        request_id: Vec<u8>,
        code: CommandErrorCode,
        message: &str,
    ) {
        let _ignored = responses.try_send(Ok(rejection(request_id, code, message)));
        let _closed = responses.try_send(Err(Status::failed_precondition(
            "OpenSession was rejected; this Controller stream carries no session",
        )));
    }
}

fn rejection(request_id: Vec<u8>, code: CommandErrorCode, message: &str) -> ServerFrame {
    ServerFrame {
        request_id,
        payload: Some(
            crate::actor_messages::server_frame::Payload::CommandRejected(CommandRejected {
                code: code.into(),
                message: message.to_owned(),
            }),
        ),
    }
}

fn validate_space_key(key: &str) -> Result<(), String> {
    SpaceKey::new(key.to_owned())
        .map(|_key| ())
        .map_err(|error| error.to_string())
}

fn decode_space_spec(
    spec: ProtocolParticipantSpec,
) -> Result<SpaceParticipantSpec, SpacesValidationError> {
    SpaceParticipantSpec::new(
        spec.space_key,
        spec.display_name,
        spec.server_mute,
        spec.server_deaf,
    )
}

fn duration_to_proto(duration: Duration) -> prost_types::Duration {
    prost_types::Duration {
        seconds: i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(duration.subsec_nanos()).unwrap_or(i32::MAX),
    }
}

fn random_bytes(length: usize) -> Result<Vec<u8>, ActorStartError> {
    let mut bytes = vec![0; length];
    getrandom::fill(&mut bytes).map_err(|_error| ActorStartError::Randomness)?;
    Ok(bytes)
}

fn random_join_token() -> Result<String, ActorStartError> {
    Ok(URL_SAFE_NO_PAD.encode(random_bytes(32)?))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ActorStartError {
    #[error("the operating system random source is unavailable")]
    Randomness,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use mumble_server_runtime_gateway::Runtime;
    use tokio::sync::mpsc;

    use super::*;
    use crate::actor_messages::client_frame::Payload as ClientPayload;
    use crate::actor_messages::server_frame::Payload as ServerPayload;
    use crate::actor_messages::{
        CloseSession, FetchSpace, OpenSession, RegisterParticipant, ReleaseParticipant, RenewLease,
        ReplaceObservedSpaces, SetParticipantSpec,
    };

    struct Harness {
        _runtime: Runtime,
        actor: ActorHandle,
        task: JoinHandle<()>,
        next_stream: u64,
    }

    impl Harness {
        fn new(mut config: ControllerConfig) -> Self {
            config.controller_bind = "127.0.0.1:0".parse().expect("loopback address");
            config.mumble_bind = "127.0.0.1:0".parse().expect("loopback address");
            let runtime = Runtime::start();
            let (actor, task) = spawn(config, runtime.handle()).expect("actor starts");
            Self {
                _runtime: runtime,
                actor,
                task,
                next_stream: 1,
            }
        }

        async fn open(
            &mut self,
            controller: &str,
            instance: Vec<u8>,
            resume_token: Vec<u8>,
            desired: DesiredStateSnapshot,
        ) -> SessionHarness {
            self.open_with(controller, instance, resume_token, desired, vec![1], 128)
                .await
        }

        async fn open_with(
            &mut self,
            controller: &str,
            instance: Vec<u8>,
            resume_token: Vec<u8>,
            desired: DesiredStateSnapshot,
            request_id: Vec<u8>,
            capacity: usize,
        ) -> SessionHarness {
            let stream_id = self.next_stream;
            self.next_stream += 1;
            let (responses, receiver) = mpsc::channel(capacity);
            let profile = profile::spaces().expect("compiled Spaces profile is valid");
            self.actor
                .sender()
                .send(ActorCommand::Open {
                    stream_id,
                    responses,
                    frame: ClientFrame {
                        request_id,
                        payload: Some(ClientPayload::OpenSession(OpenSession {
                            controller_id: controller.to_owned(),
                            controller_instance_id: instance,
                            resume_token,
                            desired_state: Some(desired),
                            profile: Some(profile::to_protocol(&profile)),
                        })),
                    },
                })
                .await
                .expect("actor accepts open");
            SessionHarness {
                actor: self.actor.clone(),
                stream_id,
                receiver,
                session_token: Vec::new(),
                resume_token: Vec::new(),
            }
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    struct SessionHarness {
        actor: ActorHandle,
        stream_id: u64,
        receiver: mpsc::Receiver<Result<ServerFrame, Status>>,
        session_token: Vec<u8>,
        resume_token: Vec<u8>,
    }

    impl SessionHarness {
        async fn next_payload(&mut self) -> ServerPayload {
            for _ in 0..256 {
                match self.receiver.try_recv() {
                    Ok(Ok(frame)) => {
                        if let Some(payload) = frame.payload {
                            if let ServerPayload::SessionReady(ready) = &payload {
                                self.session_token = ready.session_token.clone();
                                self.resume_token = ready.resume_token.clone();
                            }
                            return payload;
                        }
                    }
                    Ok(Err(status)) => panic!("gRPC status in reducer test: {status}"),
                    Err(mpsc::error::TryRecvError::Empty) => tokio::task::yield_now().await,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        panic!("response stream disconnected")
                    }
                }
            }
            panic!("actor did not produce the expected response")
        }

        async fn until(&mut self, predicate: impl Fn(&ServerPayload) -> bool) -> ServerPayload {
            for _ in 0..256 {
                let payload = self.next_payload().await;
                if predicate(&payload) {
                    return payload;
                }
            }
            panic!("actor never produced the requested payload")
        }

        async fn send(&self, request_id: u8, payload: ClientPayload) {
            self.actor
                .sender()
                .send(ActorCommand::Frame {
                    stream_id: self.stream_id,
                    frame: ClientFrame {
                        request_id: vec![request_id],
                        payload: Some(payload),
                    },
                })
                .await
                .expect("actor accepts command");
        }
    }

    fn spec(space: &str, name: &str) -> ProtocolParticipantSpec {
        ProtocolParticipantSpec {
            space_key: space.to_owned(),
            display_name: name.to_owned(),
            server_mute: false,
            server_deaf: false,
        }
    }

    fn registration(
        participant_id: &str,
        registration_id: u8,
        ownership_token: Vec<u8>,
        revision: u64,
        participant_spec: ProtocolParticipantSpec,
    ) -> ParticipantRegistration {
        ParticipantRegistration {
            participant_id: participant_id.to_owned(),
            registration_id: vec![registration_id],
            ownership_token,
            client_spec_revision: revision,
            spec: Some(participant_spec),
        }
    }

    fn snapshot(revision: u64, participants: Vec<ParticipantRegistration>) -> DesiredStateSnapshot {
        DesiredStateSnapshot {
            desired_state_revision: revision,
            participants,
            observed_space_keys: Vec::new(),
        }
    }

    async fn open_owned(harness: &mut Harness) -> (SessionHarness, ParticipantOwnershipGranted) {
        let mut session = harness
            .open(
                "controller",
                vec![9],
                Vec::new(),
                snapshot(
                    1,
                    vec![registration(
                        "alice",
                        7,
                        Vec::new(),
                        1,
                        spec("lobby", "Alice"),
                    )],
                ),
            )
            .await;
        let payload = session
            .until(|payload| matches!(payload, ServerPayload::ParticipantOwnershipGranted(_)))
            .await;
        let ServerPayload::ParticipantOwnershipGranted(grant) = payload else {
            unreachable!()
        };
        (session, grant)
    }

    #[tokio::test(start_paused = true)]
    async fn opening_reconciles_a_full_snapshot_and_issues_distinct_credentials() {
        let mut harness = Harness::new(ControllerConfig::default());
        let (mut session, grant) = open_owned(&mut harness).await;
        assert_eq!(grant.ownership_token.len(), 32);
        assert_eq!(grant.connection_credential.len(), 43);
        assert_ne!(
            grant.ownership_token,
            grant.connection_credential.as_bytes()
        );

        let barrier = session
            .until(|payload| matches!(payload, ServerPayload::DesiredStateReconciled(_)))
            .await;
        assert!(matches!(
            barrier,
            ServerPayload::DesiredStateReconciled(DesiredStateReconciled {
                desired_state_revision: 1
            })
        ));
        assert!(!session.session_token.is_empty());
        assert!(!session.resume_token.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_valid_resume_keeps_ownership_and_join_tokens() {
        let mut harness = Harness::new(ControllerConfig::default());
        let (mut first, grant) = open_owned(&mut harness).await;
        first
            .until(|payload| matches!(payload, ServerPayload::DesiredStateReconciled(_)))
            .await;
        let resume = first.resume_token.clone();

        let mut resumed = harness
            .open(
                "controller",
                vec![9],
                resume,
                snapshot(
                    1,
                    vec![registration(
                        "alice",
                        7,
                        grant.ownership_token.clone(),
                        1,
                        spec("lobby", "Alice"),
                    )],
                ),
            )
            .await;
        let payload = resumed
            .until(|payload| matches!(payload, ServerPayload::ParticipantOwnershipGranted(_)))
            .await;
        let ServerPayload::ParticipantOwnershipGranted(restored) = payload else {
            unreachable!()
        };
        assert_eq!(restored.ownership_token, grant.ownership_token);
        assert_eq!(restored.connection_credential, grant.connection_credential);
    }

    #[tokio::test(start_paused = true)]
    async fn a_snapshot_cannot_take_ownership_from_another_session() {
        let mut harness = Harness::new(ControllerConfig::default());
        let (mut owner, grant) = open_owned(&mut harness).await;
        owner
            .until(|payload| matches!(payload, ServerPayload::DesiredStateReconciled(_)))
            .await;

        let mut contender = harness
            .open(
                "controller",
                vec![10],
                Vec::new(),
                snapshot(
                    1,
                    vec![registration(
                        "alice",
                        8,
                        Vec::new(),
                        1,
                        spec("lobby", "Impostor"),
                    )],
                ),
            )
            .await;
        let revoked = contender
            .until(|payload| matches!(payload, ServerPayload::ParticipantOwnershipRevoked(_)))
            .await;
        assert!(matches!(
            revoked,
            ServerPayload::ParticipantOwnershipRevoked(ParticipantOwnershipRevoked {
                reason,
                ..
            }) if reason == i32::from(OwnershipRevocationReason::RegistrationRejected)
        ));

        let (response, awaited) = oneshot::channel();
        owner
            .actor
            .sender()
            .send(ActorCommand::Route {
                connection: ConnectionId(98),
                credential: Some(grant.connection_credential),
                response,
            })
            .await
            .expect("route reaches actor");
        assert!(awaited.await.expect("route response").is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn a_lease_watermark_mismatch_requests_a_full_resync() {
        let mut harness = Harness::new(ControllerConfig::default());
        let (mut session, _grant) = open_owned(&mut harness).await;
        session
            .until(|payload| matches!(payload, ServerPayload::DesiredStateReconciled(_)))
            .await;
        session
            .send(
                15,
                ClientPayload::RenewLease(RenewLease {
                    session_token: session.session_token.clone(),
                    desired_state_revision: 99,
                }),
            )
            .await;
        let resync = session
            .until(|payload| matches!(payload, ServerPayload::ResyncRequired(_)))
            .await;
        assert!(matches!(resync, ServerPayload::ResyncRequired(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn transfer_rotates_capabilities_and_a_stale_release_cannot_detach_it() {
        let mut harness = Harness::new(ControllerConfig::default());
        let (mut old, old_grant) = open_owned(&mut harness).await;
        old.until(|payload| matches!(payload, ServerPayload::DesiredStateReconciled(_)))
            .await;
        tokio::time::advance(mumble_server_runtime_shard::MIN_INTERVAL).await;
        old.until(|payload| {
            matches!(payload, ServerPayload::ParticipantStatusChanged(value)
                if value.status.as_ref().is_some_and(|status| status.applied_spec_revision == 1))
        })
        .await;

        let mut new = harness
            .open("controller", vec![10], Vec::new(), snapshot(0, Vec::new()))
            .await;
        new.until(|payload| matches!(payload, ServerPayload::DesiredStateReconciled(_)))
            .await;
        new.send(
            2,
            ClientPayload::RegisterParticipant(RegisterParticipant {
                session_token: new.session_token.clone(),
                participant: Some(registration(
                    "alice",
                    8,
                    Vec::new(),
                    1,
                    spec("arena", "Alice 2"),
                )),
            }),
        )
        .await;
        let payload = new
            .until(|payload| matches!(payload, ServerPayload::ParticipantOwnershipGranted(_)))
            .await;
        let ServerPayload::ParticipantOwnershipGranted(new_grant) = payload else {
            unreachable!()
        };
        assert_ne!(new_grant.ownership_token, old_grant.ownership_token);
        assert_ne!(
            new_grant.connection_credential,
            old_grant.connection_credential
        );
        assert_eq!(new_grant.accepted_spec_revision, 1);
        assert_eq!(new_grant.applied_spec_revision, 0);

        tokio::time::advance(mumble_server_runtime_shard::MIN_INTERVAL).await;
        let applied = new
            .until(|payload| {
                matches!(payload, ServerPayload::ParticipantStatusChanged(value)
                    if value.status.as_ref().is_some_and(|status|
                        status.applied_spec_revision == 1 && status.applied_space_key == "arena"))
            })
            .await;
        assert!(matches!(
            applied,
            ServerPayload::ParticipantStatusChanged(_)
        ));

        old.send(
            3,
            ClientPayload::ReleaseParticipant(ReleaseParticipant {
                session_token: old.session_token.clone(),
                participant_id: "alice".to_owned(),
                registration_id: old_grant.registration_id,
                ownership_token: old_grant.ownership_token,
            }),
        )
        .await;
        let rejected = old
            .until(|payload| matches!(payload, ServerPayload::CommandRejected(_)))
            .await;
        assert!(matches!(
            rejected,
            ServerPayload::CommandRejected(CommandRejected {
                code,
                ..
            }) if code == i32::from(CommandErrorCode::OwnershipLost)
        ));

        let (response, awaited) = oneshot::channel();
        new.actor
            .sender()
            .send(ActorCommand::Route {
                connection: ConnectionId(99),
                credential: Some(new_grant.connection_credential),
                response,
            })
            .await
            .expect("route reaches actor");
        assert!(awaited.await.expect("route response").is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn coalesced_specs_separate_acceptance_from_application_and_publication() {
        let mut harness = Harness::new(ControllerConfig::default());
        let (mut session, grant) = open_owned(&mut harness).await;
        session
            .until(|payload| matches!(payload, ServerPayload::DesiredStateReconciled(_)))
            .await;
        for revision in [2, 3] {
            session
                .send(
                    u8::try_from(revision).expect("small revision"),
                    ClientPayload::SetParticipantSpec(SetParticipantSpec {
                        session_token: session.session_token.clone(),
                        participant_id: "alice".to_owned(),
                        ownership_token: grant.ownership_token.clone(),
                        client_spec_revision: revision,
                        spec: Some(spec("lobby", &format!("Alice {revision}"))),
                    }),
                )
                .await;
        }
        let accepted = session
            .until(|payload| {
                matches!(payload, ServerPayload::ParticipantSpecAccepted(value) if value.client_spec_revision == 3)
            })
            .await;
        let ServerPayload::ParticipantSpecAccepted(accepted) = accepted else {
            unreachable!()
        };
        assert_eq!(accepted.accepted_spec_revision, 3);
        assert!(accepted.applied_spec_revision < 3);

        tokio::time::advance(mumble_server_runtime_shard::MIN_INTERVAL).await;
        let status = session
            .until(|payload| {
                matches!(payload, ServerPayload::ParticipantStatusChanged(value)
                    if value.status.as_ref().is_some_and(|status| status.applied_spec_revision == 3))
            })
            .await;
        let ServerPayload::ParticipantStatusChanged(status) = status else {
            unreachable!()
        };
        let status = status.status.expect("status exists");
        assert_eq!(status.accepted_spec_revision, 3);
        assert_eq!(status.applied_spec_revision, 3);
        assert!(status.published_generation > 0);
    }

    #[tokio::test(start_paused = true)]
    async fn lease_expiry_revokes_the_join_token_and_empty_space_gets_a_new_incarnation() {
        let config = ControllerConfig {
            lease_duration: Duration::from_secs(10),
            empty_space_grace: Duration::from_secs(5),
            ..ControllerConfig::default()
        };
        let mut harness = Harness::new(config);
        let (mut session, grant) = open_owned(&mut harness).await;
        let first_space = session
            .until(|payload| matches!(payload, ServerPayload::SpaceSnapshot(_)))
            .await;
        let ServerPayload::SpaceSnapshot(first_space) = first_space else {
            unreachable!()
        };

        tokio::time::advance(Duration::from_secs(10)).await;
        tokio::task::yield_now().await;
        let (response, awaited) = oneshot::channel();
        session
            .actor
            .sender()
            .send(ActorCommand::Route {
                connection: ConnectionId(100),
                credential: Some(grant.connection_credential),
                response,
            })
            .await
            .expect("route reaches actor");
        assert!(awaited.await.expect("route response").is_err());

        tokio::time::advance(Duration::from_secs(5)).await;
        tokio::task::yield_now().await;
        let mut replacement = harness
            .open(
                "controller",
                vec![11],
                Vec::new(),
                snapshot(
                    1,
                    vec![registration("bob", 9, Vec::new(), 1, spec("lobby", "Bob"))],
                ),
            )
            .await;
        let second_space = replacement
            .until(|payload| matches!(payload, ServerPayload::SpaceSnapshot(_)))
            .await;
        let ServerPayload::SpaceSnapshot(second_space) = second_space else {
            unreachable!()
        };
        assert_ne!(first_space.incarnation_id, second_space.incarnation_id);
    }

    #[tokio::test(start_paused = true)]
    async fn closing_is_immediate_and_idempotent_at_the_process_boundary() {
        let mut harness = Harness::new(ControllerConfig::default());
        let (mut session, _grant) = open_owned(&mut harness).await;
        session
            .until(|payload| matches!(payload, ServerPayload::DesiredStateReconciled(_)))
            .await;
        session
            .send(
                4,
                ClientPayload::CloseSession(CloseSession {
                    session_token: session.session_token.clone(),
                }),
            )
            .await;
        let closing = session
            .until(|payload| matches!(payload, ServerPayload::SessionClosing(_)))
            .await;
        assert!(matches!(closing, ServerPayload::SessionClosing(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn request_ids_replay_the_release_result_without_touching_new_state() {
        let mut harness = Harness::new(ControllerConfig::default());
        let (mut session, grant) = open_owned(&mut harness).await;
        session
            .until(|payload| matches!(payload, ServerPayload::DesiredStateReconciled(_)))
            .await;
        let release = ClientPayload::ReleaseParticipant(ReleaseParticipant {
            session_token: session.session_token.clone(),
            participant_id: "alice".to_owned(),
            registration_id: grant.registration_id,
            ownership_token: grant.ownership_token,
        });
        session.send(12, release.clone()).await;
        let first = session
            .until(|payload| matches!(payload, ServerPayload::ParticipantOwnershipRevoked(_)))
            .await;
        session.send(12, release).await;
        let replay = session
            .until(|payload| matches!(payload, ServerPayload::ParticipantOwnershipRevoked(_)))
            .await;
        assert_eq!(first, replay);
    }

    /// An actor driven directly, with no spawned loop and no clock.
    ///
    /// These reducer steps are ordering bugs. Reproducing them through the
    /// spawned actor would race the real shard report bridges, so the state
    /// machine is exercised in place instead.
    fn reducer(runtime: &Runtime) -> (ControllerActor, mpsc::Receiver<ActorCommand>) {
        let (sender, receiver) = mpsc::channel(64);
        let config = ControllerConfig::default();
        let core_sessions = SessionRegistry::new(config.max_sessions, config.lease_duration);
        let ownership =
            OwnershipRegistry::new(config.max_participants, config.max_participants_per_session);
        let reliable = ReliableCommands::new(config.queue_capacity);
        let actor = ControllerActor {
            config,
            host: MumbleHost::new(runtime.handle()),
            sender,
            control_epoch: vec![0; 16],
            core_sessions,
            ownership,
            reliable,
            sessions: HashMap::new(),
            streams: HashMap::new(),
            participants: BTreeMap::new(),
            spaces: BTreeMap::new(),
            next_application_revision: 1,
        };
        (actor, receiver)
    }

    fn attached_session(
        actor: &mut ControllerActor,
        capacity: usize,
    ) -> mpsc::Receiver<Result<ServerFrame, Status>> {
        let (responses, receiver) = mpsc::channel(capacity);
        let ready = actor
            .core_sessions
            .open(CoreOpenSession {
                controller_id: "controller",
                controller_instance_id: &[9],
                resume_token: &[],
                profile: profile::spaces().expect("compiled Spaces profile is valid"),
                credentials: SessionCredentials::new(vec![7; 32], vec![8; 32])
                    .expect("valid test credentials"),
                now: Instant::now().into_std(),
            })
            .expect("create attached test session");
        actor
            .core_sessions
            .set_desired_revision(ready.session_id, 3);
        actor.reliable.attach_session(ready.session_id);
        actor.sessions.insert(
            ready.session_id,
            ControllerSession {
                stream_id: Some(5),
                responses: Some(responses),
                spaces: SpacesSessionState::default(),
            },
        );
        actor.streams.insert(5, ready.session_id);
        receiver
    }

    fn insert_owned_participant(actor: &mut ControllerActor, space_key: &str) {
        actor
            .ownership
            .claim(ClaimRequest {
                entity_id: "alice",
                owner: 1,
                registration_id: &[7],
                presented_fencing_token: &[],
                client_revision: 1,
                new_fencing_token: vec![1; 32],
                mode: ClaimMode::Explicit,
            })
            .expect("claim test participant");
        let participant = Participant {
            participant_id: "alice".to_owned(),
            applied_spec_revision: 0,
            applied_space_key: None,
            published_generation: 0,
            spec: decode_space_spec(spec(space_key, "Alice")).expect("valid Spaces test spec"),
            self_mute: false,
            self_deaf: false,
            application_error: String::new(),
        };
        actor
            .host
            .register("alice", "join".to_owned())
            .expect("register test Mumble credential");
        actor.participants.insert("alice".to_owned(), participant);
    }

    fn payloads(receiver: &mut mpsc::Receiver<Result<ServerFrame, Status>>) -> Vec<ServerPayload> {
        let mut drained = Vec::new();
        while let Ok(Ok(frame)) = receiver.try_recv() {
            if let Some(payload) = frame.payload {
                drained.push(payload);
            }
        }
        drained
    }

    /// A report rendered before a move must not republish the Space left behind.
    ///
    /// Each Space reports through its own bridge task, so the order in which two
    /// Spaces reach the actor is arbitrary. Applying the stale one pins
    /// `applied_space_key` to a Space the participant no longer belongs to, and
    /// nothing corrects it until the destination Space renders again.
    #[tokio::test]
    async fn a_stale_report_from_the_previous_space_is_not_applied() {
        let runtime = Runtime::start();
        let (mut actor, _commands) = reducer(&runtime);
        actor.ensure_space("lobby").expect("lobby materializes");
        actor.ensure_space("arena").expect("arena materializes");
        insert_owned_participant(&mut actor, "lobby");

        actor.refresh_space("lobby");
        let lobby_revision = actor.next_application_revision - 1;
        if let Some(participant) = actor.participants.get_mut("alice") {
            participant.spec =
                decode_space_spec(spec("arena", "Alice")).expect("valid Spaces test spec");
        }
        actor.refresh_space("arena");
        let arena_revision = actor.next_application_revision - 1;

        actor.reconciled(
            "arena",
            arena_revision,
            ReconcileReport {
                version: 9,
                ..ReconcileReport::default()
            },
        );
        assert_eq!(
            actor.participants["alice"].applied_space_key.as_deref(),
            Some("arena")
        );

        actor.reconciled(
            "lobby",
            lobby_revision,
            ReconcileReport {
                version: 4,
                ..ReconcileReport::default()
            },
        );
        assert_eq!(
            actor.participants["alice"].applied_space_key.as_deref(),
            Some("arena"),
            "a report rendered before the move republished the Space Alice left"
        );
        assert_eq!(
            actor.participants["alice"].published_generation, 9,
            "the stale report also rewound the published generation"
        );
    }

    /// A full response queue must not silently detach a live stream.
    ///
    /// Detaching drops every later command, `RenewLease` included, so the lease
    /// expires on a controller that was never told anything went wrong. The
    /// session stays attached and the next renewal demands a resync instead.
    #[tokio::test]
    async fn a_full_response_queue_demands_a_resync_instead_of_muting_the_stream() {
        let runtime = Runtime::start();
        let (mut actor, _commands) = reducer(&runtime);
        let mut client = attached_session(&mut actor, 1);

        actor.send_session_uncached(1, rejection(vec![1], CommandErrorCode::NotFound, "first"));
        actor.send_session_uncached(1, rejection(vec![2], CommandErrorCode::NotFound, "dropped"));
        assert_eq!(
            payloads(&mut client).len(),
            1,
            "the second frame overflowed"
        );
        assert!(
            actor.streams.contains_key(&5),
            "a momentarily full queue is not a dead stream"
        );

        actor.frame(
            5,
            ClientFrame {
                request_id: vec![3],
                payload: Some(ClientPayload::RenewLease(RenewLease {
                    session_token: vec![7; 32],
                    desired_state_revision: 3,
                })),
            },
        );
        let answered = payloads(&mut client);
        assert!(
            answered
                .iter()
                .any(|payload| matches!(payload, ServerPayload::ResyncRequired(_))),
            "the renewal was swallowed, so this lease expires in silence: {answered:?}"
        );
    }

    /// A closed response queue is a dead stream and must be detached.
    #[tokio::test]
    async fn a_closed_response_queue_detaches_the_stream() {
        let runtime = Runtime::start();
        let (mut actor, _commands) = reducer(&runtime);
        let client = attached_session(&mut actor, 4);
        drop(client);

        actor.send_session_uncached(1, rejection(vec![1], CommandErrorCode::NotFound, "gone"));
        assert!(!actor.streams.contains_key(&5));
        assert!(
            actor.sessions[&1].responses.is_none(),
            "a closed stream must not stay attached to the session"
        );
    }

    /// A rejected `OpenSession` must end the stream.
    ///
    /// No session maps to this stream, so every later frame would be dropped in
    /// `frame` without an answer. Terminating the response stream is the only
    /// outcome the controller can observe.
    #[tokio::test]
    async fn a_rejected_open_session_terminates_the_stream() {
        let runtime = Runtime::start();
        let (mut actor, _commands) = reducer(&runtime);
        let (responses, mut client) = mpsc::channel(8);

        actor.open(
            5,
            responses,
            ClientFrame {
                request_id: Vec::new(),
                payload: Some(ClientPayload::OpenSession(OpenSession {
                    controller_id: "controller".to_owned(),
                    controller_instance_id: vec![9],
                    resume_token: Vec::new(),
                    desired_state: Some(snapshot(1, Vec::new())),
                    profile: None,
                })),
            },
        );

        assert!(matches!(
            client.try_recv(),
            Ok(Ok(ServerFrame {
                payload: Some(ServerPayload::CommandRejected(_)),
                ..
            }))
        ));
        assert!(
            matches!(client.try_recv(), Ok(Err(_status))),
            "the stream carries no session, so it must not stay open in silence"
        );
    }

    #[tokio::test]
    async fn an_unknown_profile_is_rejected_before_session_creation() {
        let runtime = Runtime::start();
        let (mut actor, _commands) = reducer(&runtime);
        let (responses, mut client) = mpsc::channel(8);
        let supported = profile::spaces().expect("compiled Spaces profile is valid");
        let mut unknown = profile::to_protocol(&supported);
        unknown.profile_id = "unknown".to_owned();

        actor.open(
            5,
            responses,
            ClientFrame {
                request_id: vec![1],
                payload: Some(ClientPayload::OpenSession(OpenSession {
                    controller_id: "controller".to_owned(),
                    controller_instance_id: vec![9],
                    resume_token: Vec::new(),
                    desired_state: Some(snapshot(1, Vec::new())),
                    profile: Some(unknown),
                })),
            },
        );

        assert!(actor.sessions.is_empty());
        assert!(actor.core_sessions.is_empty());
        assert!(actor.streams.is_empty());
        assert!(matches!(
            client.try_recv(),
            Ok(Ok(ServerFrame {
                payload: Some(ServerPayload::CommandRejected(_)),
                ..
            }))
        ));
        assert!(matches!(client.try_recv(), Ok(Err(_status))));
    }

    #[tokio::test]
    async fn a_missing_profile_is_rejected_before_session_creation() {
        let runtime = Runtime::start();
        let (mut actor, _commands) = reducer(&runtime);
        let (responses, mut client) = mpsc::channel(8);

        actor.open(
            5,
            responses,
            ClientFrame {
                request_id: vec![1],
                payload: Some(ClientPayload::OpenSession(OpenSession {
                    controller_id: "controller".to_owned(),
                    controller_instance_id: vec![9],
                    resume_token: Vec::new(),
                    desired_state: Some(snapshot(1, Vec::new())),
                    profile: None,
                })),
            },
        );

        assert!(actor.sessions.is_empty());
        assert!(actor.core_sessions.is_empty());
        assert!(actor.streams.is_empty());
        assert!(matches!(
            client.try_recv(),
            Ok(Ok(ServerFrame {
                payload: Some(ServerPayload::CommandRejected(_)),
                ..
            }))
        ));
        assert!(matches!(client.try_recv(), Ok(Err(_status))));
    }

    #[tokio::test(start_paused = true)]
    async fn observation_and_fetch_do_not_materialize_an_absent_space() {
        let mut harness = Harness::new(ControllerConfig::default());
        let mut session = harness
            .open("observer", vec![12], Vec::new(), snapshot(0, Vec::new()))
            .await;
        session
            .until(|payload| matches!(payload, ServerPayload::DesiredStateReconciled(_)))
            .await;
        session
            .send(
                13,
                ClientPayload::ReplaceObservedSpaces(ReplaceObservedSpaces {
                    session_token: session.session_token.clone(),
                    observed_spaces_revision: 1,
                    space_keys: vec!["absent".to_owned()],
                }),
            )
            .await;
        session
            .until(|payload| matches!(payload, ServerPayload::ObservedSpacesAccepted(_)))
            .await;
        session
            .send(
                14,
                ClientPayload::FetchSpace(FetchSpace {
                    session_token: session.session_token.clone(),
                    space_key: "absent".to_owned(),
                }),
            )
            .await;
        let result = session
            .until(|payload| matches!(payload, ServerPayload::FetchSpaceResult(_)))
            .await;
        assert!(matches!(
            result,
            ServerPayload::FetchSpaceResult(FetchSpaceResult {
                result: Some(FetchResult::Absent(_))
            })
        ));
    }
}
