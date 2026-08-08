use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use mumble_server_runtime_gateway::RuntimeHandle;
use mumble_server_runtime_shard::{ConnectionId, ReconcileReport, ShardHandle, ShardId};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tonic::Status;

use crate::config::ControllerConfig;
use crate::protocol::client_frame::Payload as ClientPayload;
use crate::protocol::fetch_space_result::Result as FetchResult;
use crate::protocol::{
    ClientFrame, CommandErrorCode, CommandRejected, DesiredStateReconciled, DesiredStateSnapshot,
    FetchSpaceResult, ObservedSpacesAccepted, OpenSession, OwnershipRevocationReason,
    ParticipantOwnershipGranted, ParticipantOwnershipRevoked, ParticipantRegistration,
    ParticipantSpec, ParticipantSpecAccepted, ParticipantStatus, ParticipantStatusChanged,
    ResyncRequired, ServerFrame, SessionClosing, SessionReady, SpaceAbsent, SpaceClosed,
    SpaceParticipant, SpaceSnapshot,
};
use crate::space::{ControllerSpaceLogic, RenderParticipant, RenderState};

pub(crate) type ResponseSender = mpsc::Sender<Result<ServerFrame, Status>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpaceEvent {
    Connected(ConnectionId),
    Disconnected(ConnectionId),
    SelfState {
        connection: ConnectionId,
        self_mute: bool,
        self_deaf: bool,
    },
}

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
    SpaceEvent {
        space_key: String,
        event: SpaceEvent,
    },
    Reconciled {
        space_key: String,
        application_revision: u64,
        report: ReconcileReport,
    },
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
    let actor = ControllerActor {
        config,
        runtime,
        sender: sender.clone(),
        control_epoch,
        sessions: HashMap::new(),
        resume_tokens: HashMap::new(),
        streams: HashMap::new(),
        participants: BTreeMap::new(),
        join_tokens: HashMap::new(),
        spaces: BTreeMap::new(),
        next_session_id: 1,
        next_application_revision: 1,
    };
    let task = tokio::spawn(actor.run(receiver));
    Ok((ActorHandle { sender }, task))
}

type SessionId = u64;

struct ControllerSession {
    controller_id: String,
    instance_id: Vec<u8>,
    session_token: Vec<u8>,
    resume_token: Vec<u8>,
    expires_at: Instant,
    stream_id: Option<u64>,
    responses: Option<ResponseSender>,
    desired_revision: u64,
    observed_revision: u64,
    explicit_observations: BTreeSet<String>,
    completed_requests: HashMap<Vec<u8>, ServerFrame>,
    completed_order: VecDeque<Vec<u8>>,
    needs_resync: bool,
}

#[derive(Clone)]
struct Participant {
    participant_id: String,
    owner: SessionId,
    registration_id: Vec<u8>,
    ownership_token: Vec<u8>,
    join_token: String,
    client_spec_revision: u64,
    accepted_spec_revision: u64,
    applied_spec_revision: u64,
    applied_space_key: Option<String>,
    published_generation: u64,
    spec: ParticipantSpec,
    connection: Option<ConnectionId>,
    self_mute: bool,
    self_deaf: bool,
    application_error: String,
}

struct Space {
    handle: ShardHandle,
    incarnation_id: Vec<u8>,
    space_revision: u64,
    published_generation: u64,
    desired: watch::Sender<Arc<RenderState>>,
    render_history: BTreeMap<u64, BTreeMap<String, u64>>,
    close_deadline: Option<Instant>,
}

struct ControllerActor {
    config: ControllerConfig,
    runtime: RuntimeHandle,
    sender: mpsc::Sender<ActorCommand>,
    control_epoch: Vec<u8>,
    sessions: HashMap<SessionId, ControllerSession>,
    resume_tokens: HashMap<Vec<u8>, SessionId>,
    streams: HashMap<u64, SessionId>,
    participants: BTreeMap<String, Participant>,
    join_tokens: HashMap<String, String>,
    spaces: BTreeMap<String, Space>,
    next_session_id: SessionId,
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
            ActorCommand::SpaceEvent { space_key, event } => {
                self.space_event(&space_key, event);
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
        if let Err(message) = self.validate_snapshot(&open.desired_state) {
            self.reject_stream(
                &responses,
                request_id,
                CommandErrorCode::InvalidArgument,
                &message,
            );
            return;
        }

        let now = Instant::now();
        let resumed = self
            .resume_tokens
            .get(&open.resume_token)
            .copied()
            .filter(|session_id| {
                self.sessions.get(session_id).is_some_and(|session| {
                    session.controller_id == open.controller_id
                        && session.instance_id == open.controller_instance_id
                        && session.expires_at > now
                })
            });
        let session_id = match resumed {
            Some(session_id) => session_id,
            None => {
                if self.sessions.len() >= self.config.max_sessions {
                    self.reject_stream(
                        &responses,
                        request_id,
                        CommandErrorCode::ResourceExhausted,
                        "the Controller session limit is reached",
                    );
                    return;
                }
                match self.create_session(&open, now) {
                    Ok(session_id) => session_id,
                    Err(error) => {
                        self.reject_stream(
                            &responses,
                            request_id,
                            CommandErrorCode::InternalError,
                            &error.to_string(),
                        );
                        return;
                    }
                }
            }
        };

        self.attach_stream(session_id, stream_id, responses);
        let ready = match self.session_ready(session_id) {
            Ok(ready) => ready,
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
        self.send_session(
            session_id,
            ServerFrame {
                request_id: request_id.clone(),
                payload: Some(crate::protocol::server_frame::Payload::SessionReady(ready)),
            },
        );
        self.reconcile_snapshot(session_id, open.desired_state, request_id);
    }

    fn create_session(
        &mut self,
        open: &OpenSession,
        now: Instant,
    ) -> Result<SessionId, ActorStartError> {
        let session_id = self.next_session_id;
        self.next_session_id = self.next_session_id.saturating_add(1);
        let session_token = random_bytes(32)?;
        let resume_token = random_bytes(32)?;
        self.resume_tokens.insert(resume_token.clone(), session_id);
        self.sessions.insert(
            session_id,
            ControllerSession {
                controller_id: open.controller_id.clone(),
                instance_id: open.controller_instance_id.clone(),
                session_token,
                resume_token,
                expires_at: now + self.config.lease_duration,
                stream_id: None,
                responses: None,
                desired_revision: 0,
                observed_revision: 0,
                explicit_observations: BTreeSet::new(),
                completed_requests: HashMap::new(),
                completed_order: VecDeque::new(),
                needs_resync: false,
            },
        );
        Ok(session_id)
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
            session.expires_at = Instant::now() + self.config.lease_duration;
        }
    }

    fn session_ready(&mut self, session_id: SessionId) -> Result<SessionReady, ActorStartError> {
        let new_session_token = random_bytes(32)?;
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return Err(ActorStartError::MissingSession(session_id));
        };
        session.session_token = new_session_token.clone();
        Ok(SessionReady {
            session_token: new_session_token,
            resume_token: session.resume_token.clone(),
            control_epoch: self.control_epoch.clone(),
            lease_duration: Some(duration_to_proto(self.config.lease_duration)),
        })
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
        if let Some(cached) = self
            .sessions
            .get(&session_id)
            .and_then(|session| session.completed_requests.get(&request_id))
            .cloned()
        {
            self.send_session_uncached(session_id, cached);
            return;
        }

        match payload {
            ClientPayload::RenewLease(command) => {
                if !self.authorized(session_id, &command.session_token) {
                    self.reject_session(
                        session_id,
                        request_id,
                        CommandErrorCode::SessionExpiredError,
                        "session token is not current",
                    );
                    return;
                }
                if let Some(session) = self.sessions.get_mut(&session_id) {
                    session.expires_at = Instant::now() + self.config.lease_duration;
                }
                // `needs_resync` means the server already dropped at least one frame
                // this session never saw, so its replica cannot be trusted even when
                // the watermarks still agree.
                let stale = self.sessions.get_mut(&session_id).is_some_and(|session| {
                    let stale = session.needs_resync
                        || session.desired_revision != command.desired_state_revision;
                    session.needs_resync = false;
                    stale
                });
                if stale {
                    self.send_session(
                        session_id,
                        ServerFrame {
                            request_id,
                            payload: Some(crate::protocol::server_frame::Payload::ResyncRequired(
                                ResyncRequired {
                                    reason: "the server replica no longer matches this session"
                                        .to_owned(),
                                },
                            )),
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
                if let Err(message) = validate_spec(&spec) {
                    self.reject_session(
                        session_id,
                        request_id,
                        CommandErrorCode::InvalidArgument,
                        &message,
                    );
                    return;
                }
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
                        payload: Some(crate::protocol::server_frame::Payload::FetchSpaceResult(
                            FetchSpaceResult {
                                result: Some(result),
                            },
                        )),
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
                        payload: Some(crate::protocol::server_frame::Payload::SessionClosing(
                            SessionClosing {
                                reason: "session closed by controller".to_owned(),
                            },
                        )),
                    },
                );
                self.remove_session(session_id, OwnershipRevocationReason::ParticipantReleased);
            }
            ClientPayload::OpenSession(_) => {}
        }
    }

    fn authorized(&self, session_id: SessionId, token: &[u8]) -> bool {
        self.sessions
            .get(&session_id)
            .is_some_and(|session| session.session_token == token)
    }

    fn advance_desired_revision(&mut self, session_id: SessionId) {
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.desired_revision = session.desired_revision.saturating_add(1);
        }
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
        validate_spec(spec)
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
            .participants
            .values()
            .filter(|participant| {
                participant.owner == session_id
                    && !desired_ids.contains(&participant.participant_id)
            })
            .map(|participant| participant.participant_id.clone())
            .collect();
        for participant_id in no_longer_desired {
            let Some(participant) = self.participants.get(&participant_id).cloned() else {
                continue;
            };
            self.release(
                session_id,
                &participant_id,
                &participant.registration_id,
                &participant.ownership_token,
                OwnershipRevocationReason::ParticipantReleased,
                None,
            );
        }

        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.desired_revision = snapshot.desired_state_revision;
            session.explicit_observations = snapshot.observed_space_keys.into_iter().collect();
        }
        for registration in snapshot.participants {
            self.register_from_snapshot(session_id, registration, request_id.clone());
        }
        self.send_session(
            session_id,
            ServerFrame {
                request_id,
                payload: Some(
                    crate::protocol::server_frame::Payload::DesiredStateReconciled(
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
        if let Some(current) = self.participants.get(&registration.participant_id).cloned() {
            if current.owner == session_id
                && current.registration_id == registration.registration_id
                && current.ownership_token == registration.ownership_token
            {
                self.resume_registration(current, registration, request_id);
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
        self.acquire(session_id, registration, request_id);
    }

    fn register_explicit(
        &mut self,
        session_id: SessionId,
        registration: ParticipantRegistration,
        request_id: Vec<u8>,
    ) {
        if let Some(current) = self.participants.get(&registration.participant_id).cloned()
            && current.owner == session_id
            && current.registration_id == registration.registration_id
        {
            self.resume_registration(current, registration, request_id);
            return;
        }
        self.acquire(session_id, registration, request_id);
    }

    fn resume_registration(
        &mut self,
        current: Participant,
        registration: ParticipantRegistration,
        request_id: Vec<u8>,
    ) {
        if registration.client_spec_revision > current.client_spec_revision
            && let Some(spec) = registration.spec
        {
            self.apply_spec(
                &current.participant_id,
                registration.client_spec_revision,
                spec,
            );
        }
        if let Some(participant) = self.participants.get(&current.participant_id).cloned() {
            self.send_grant(participant.owner, &participant, request_id);
        }
    }

    fn acquire(
        &mut self,
        session_id: SessionId,
        registration: ParticipantRegistration,
        request_id: Vec<u8>,
    ) {
        let existing = self.participants.get(&registration.participant_id).cloned();
        if existing.is_none() && self.participants.len() >= self.config.max_participants {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::ResourceExhausted,
                "the global participant limit is reached",
            );
            return;
        }
        let owned = self
            .participants
            .values()
            .filter(|participant| participant.owner == session_id)
            .count();
        if existing
            .as_ref()
            .is_none_or(|participant| participant.owner != session_id)
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
        let Some(spec) = registration.spec else {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::InvalidArgument,
                "participant spec is missing",
            );
            return;
        };
        if let Err(error) = self.ensure_space(&spec.space_key) {
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
        let mut connection = None;
        let mut self_mute = false;
        let mut self_deaf = false;
        let mut applied_space_key = None;
        let mut published_generation = 0;
        if let Some(previous) = existing {
            connection = previous.connection;
            self_mute = previous.self_mute;
            self_deaf = previous.self_deaf;
            applied_space_key = previous.applied_space_key.clone();
            published_generation = previous.published_generation;
            self.join_tokens.remove(&previous.join_token);
            self.advance_desired_revision(previous.owner);
            self.send_revocation(
                previous.owner,
                &previous.participant_id,
                previous.registration_id,
                OwnershipRevocationReason::OwnershipReplaced,
                Vec::new(),
            );
        }
        let participant = Participant {
            participant_id: registration.participant_id.clone(),
            owner: session_id,
            registration_id: registration.registration_id,
            ownership_token,
            join_token: join_token.clone(),
            client_spec_revision: registration.client_spec_revision,
            accepted_spec_revision: registration.client_spec_revision,
            // Client revisions belong to one registration capability. A transfer
            // keeps the live connection, not the previous owner's revision space.
            applied_spec_revision: 0,
            applied_space_key,
            published_generation,
            spec,
            connection,
            self_mute,
            self_deaf,
            application_error: String::new(),
        };
        self.join_tokens
            .insert(join_token, participant.participant_id.clone());
        let old_space = self
            .participants
            .insert(participant.participant_id.clone(), participant.clone())
            .map(|previous| previous.spec.space_key);
        if let Some(old_space) = old_space
            && old_space != participant.spec.space_key
        {
            self.refresh_space(&old_space);
            self.refresh_space(&participant.spec.space_key);
            if let Some(connection) = participant.connection
                && let Some(space) = self.spaces.get(&participant.spec.space_key)
            {
                self.runtime
                    .move_connection(connection, space.handle.shard());
            }
        } else {
            self.refresh_space(&participant.spec.space_key);
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
        spec: ParticipantSpec,
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
        if current.owner != session_id || current.ownership_token != ownership_token {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::OwnershipLost,
                "participant ownership token is stale",
            );
            return;
        }
        if client_revision < current.client_spec_revision {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::StaleRevision,
                "participant spec revision moved backwards",
            );
            return;
        }
        if client_revision == current.client_spec_revision && spec != current.spec {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::StaleRevision,
                "a participant spec cannot change without advancing its revision",
            );
            return;
        }
        if let Err(error) = self.ensure_space(&spec.space_key) {
            self.reject_session(
                session_id,
                request_id,
                CommandErrorCode::ResourceExhausted,
                &error,
            );
            return;
        }
        if client_revision > current.client_spec_revision {
            self.apply_spec(&participant_id, client_revision, spec);
        }
        let Some(participant) = self.participants.get(&participant_id).cloned() else {
            return;
        };
        self.send_session(
            session_id,
            ServerFrame {
                request_id,
                payload: Some(
                    crate::protocol::server_frame::Payload::ParticipantSpecAccepted(
                        ParticipantSpecAccepted {
                            participant_id,
                            client_spec_revision: participant.client_spec_revision,
                            accepted_spec_revision: participant.accepted_spec_revision,
                            applied_spec_revision: participant.applied_spec_revision,
                            published_generation: participant.published_generation,
                        },
                    ),
                ),
            },
        );
    }

    fn apply_spec(&mut self, participant_id: &str, client_revision: u64, spec: ParticipantSpec) {
        let Some(current) = self.participants.get(participant_id).cloned() else {
            return;
        };
        let old_space = current.spec.space_key;
        let new_space = spec.space_key.clone();
        let connection = current.connection;
        if let Some(participant) = self.participants.get_mut(participant_id) {
            participant.client_spec_revision = client_revision;
            participant.accepted_spec_revision = client_revision;
            participant.spec = spec;
        }
        self.refresh_space(&old_space);
        if new_space != old_space {
            self.refresh_space(&new_space);
            if let Some(connection) = connection
                && let Some(space) = self.spaces.get(&new_space)
            {
                self.runtime
                    .move_connection(connection, space.handle.shard());
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
        let Some(current) = self.participants.get(participant_id).cloned() else {
            if let Some(request_id) = request_id {
                self.reject_session(
                    session_id,
                    request_id,
                    CommandErrorCode::OwnershipLost,
                    "participant is no longer owned",
                );
            }
            return;
        };
        if current.owner != session_id
            || current.registration_id != registration_id
            || current.ownership_token != ownership_token
        {
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
        self.participants.remove(participant_id);
        self.join_tokens.remove(&current.join_token);
        if let Some(connection) = current.connection
            && let Some(peer) = self.runtime.peers().by_connection(connection)
        {
            peer.close();
        }
        self.refresh_space(&current.spec.space_key);
        self.send_revocation(
            session_id,
            participant_id,
            current.registration_id,
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
        if let Some(session) = self.sessions.get_mut(&session_id) {
            if revision < session.observed_revision {
                self.reject_session(
                    session_id,
                    request_id,
                    CommandErrorCode::StaleRevision,
                    "observation revision moved backwards",
                );
                return;
            }
            session.observed_revision = revision;
            session.explicit_observations = set;
        }
        self.send_session(
            session_id,
            ServerFrame {
                request_id,
                payload: Some(
                    crate::protocol::server_frame::Payload::ObservedSpacesAccepted(
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
        let rendered_application_revision = Arc::new(AtomicU64::new(0));
        let initial = Arc::new(RenderState {
            application_revision: 0,
            space_key: space_key.to_owned(),
            participants: Vec::new(),
        });
        let (desired, receiver) = watch::channel(initial);
        let actor_for_logic = self.sender.clone();
        let actor_for_report = self.sender.clone();
        let key_for_logic = space_key.to_owned();
        let key_for_report = space_key.to_owned();
        let revision_for_logic = Arc::clone(&rendered_application_revision);
        let revision_for_report = Arc::clone(&rendered_application_revision);
        let (report_sender, mut report_receiver) = watch::channel(None::<(u64, ReconcileReport)>);
        let handle = self.runtime.create_shard_with_reports(
            move |_handle| {
                ControllerSpaceLogic::new(
                    key_for_logic,
                    receiver,
                    revision_for_logic,
                    actor_for_logic,
                )
            },
            move |report| {
                report_sender.send_replace(Some((
                    revision_for_report.load(Ordering::SeqCst),
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
            Space {
                handle,
                incarnation_id,
                space_revision: 0,
                published_generation: 0,
                desired,
                render_history: BTreeMap::new(),
                close_deadline: None,
            },
        );
        Ok(())
    }

    fn refresh_space(&mut self, space_key: &str) {
        let participants: Vec<RenderParticipant> = self
            .participants
            .values()
            .filter(|participant| participant.spec.space_key == space_key)
            .map(|participant| RenderParticipant {
                participant_id: participant.participant_id.clone(),
                connection: participant.connection,
                display_name: participant.spec.display_name.clone(),
                server_mute: participant.spec.server_mute,
                server_deaf: participant.spec.server_deaf,
                self_mute: participant.self_mute,
                self_deaf: participant.self_deaf,
                accepted_revision: participant.accepted_spec_revision,
            })
            .collect();
        let revisions: BTreeMap<String, u64> = participants
            .iter()
            .map(|participant| {
                (
                    participant.participant_id.clone(),
                    participant.accepted_revision,
                )
            })
            .collect();
        let application_revision = self.next_application_revision;
        // Saturation is an explicit terminal watermark: it preserves ordering instead of wrapping.
        self.next_application_revision = self.next_application_revision.saturating_add(1);
        let Some(space) = self.spaces.get_mut(space_key) else {
            return;
        };
        space.space_revision = space.space_revision.saturating_add(1);
        space.close_deadline = if participants.is_empty() {
            space
                .close_deadline
                .or(Some(Instant::now() + self.config.empty_space_grace))
        } else {
            None
        };
        space.render_history.insert(application_revision, revisions);
        space.desired.send_replace(Arc::new(RenderState {
            application_revision,
            space_key: space_key.to_owned(),
            participants,
        }));
        space.handle.wake();
        self.broadcast_space_snapshot(space_key);
    }

    fn reconciled(&mut self, space_key: &str, application_revision: u64, report: ReconcileReport) {
        let Some(space) = self.spaces.get_mut(space_key) else {
            return;
        };
        let Some(revisions) = space.render_history.get(&application_revision).cloned() else {
            return;
        };
        let refused = report.refused.as_ref().map(ToString::to_string);
        if refused.is_none() {
            space.published_generation = report.version;
        }
        space
            .render_history
            .retain(|revision, _| *revision > application_revision);
        let participant_ids: Vec<String> = revisions.keys().cloned().collect();
        for (participant_id, applied_revision) in revisions {
            if let Some(participant) = self.participants.get_mut(&participant_id) {
                // Each Space reports through its own bridge task, so a report rendered
                // before a move can reach the actor after the destination Space has
                // already reported. Applying it would publish a Space the participant
                // has left, and nothing would correct it until the next render there.
                if participant.spec.space_key != space_key {
                    continue;
                }
                match &refused {
                    Some(error) => participant.application_error = error.clone(),
                    None => {
                        participant.applied_spec_revision =
                            participant.applied_spec_revision.max(applied_revision);
                        participant.applied_space_key = Some(space_key.to_owned());
                        participant.published_generation = report.version;
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
        let Some(participant_id) = self.join_tokens.get(&credential).cloned() else {
            let _ignored =
                response.send(Err("the Mumble join token is invalid or revoked".to_owned()));
            return;
        };
        let Some(current) = self.participants.get(&participant_id).cloned() else {
            let _ignored =
                response.send(Err("the Mumble join token is invalid or revoked".to_owned()));
            return;
        };
        if let Some(previous) = current.connection
            && previous != connection
            && let Some(peer) = self.runtime.peers().by_connection(previous)
        {
            peer.close();
        }
        if let Some(participant) = self.participants.get_mut(&participant_id) {
            participant.connection = Some(connection);
        }
        self.refresh_space(&current.spec.space_key);
        self.send_status(&participant_id);
        let result = self
            .spaces
            .get(&current.spec.space_key)
            .map(|space| space.handle.shard())
            .ok_or_else(|| "the participant Space is not materialized".to_owned());
        let _ignored = response.send(result);
    }

    fn space_event(&mut self, space_key: &str, event: SpaceEvent) {
        let connection = match event {
            SpaceEvent::Connected(connection)
            | SpaceEvent::Disconnected(connection)
            | SpaceEvent::SelfState { connection, .. } => connection,
        };
        let participant_id = self
            .participants
            .values()
            .find(|participant| participant.connection == Some(connection))
            .map(|participant| participant.participant_id.clone());
        let Some(participant_id) = participant_id else {
            return;
        };
        let mut refresh = false;
        if let Some(participant) = self.participants.get_mut(&participant_id) {
            if participant.spec.space_key != space_key {
                return;
            }
            match event {
                SpaceEvent::Connected(_) => {}
                SpaceEvent::Disconnected(_) => {
                    participant.connection = None;
                    refresh = true;
                }
                SpaceEvent::SelfState {
                    self_mute,
                    self_deaf,
                    ..
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
        self.sessions
            .values()
            .map(|session| session.expires_at)
            .chain(
                self.spaces
                    .values()
                    .filter_map(|space| space.close_deadline),
            )
            .min()
    }

    fn expire_due(&mut self) {
        let now = Instant::now();
        let expired_sessions: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, session)| session.expires_at <= now)
            .map(|(session_id, _)| *session_id)
            .collect();
        for session_id in expired_sessions {
            self.remove_session(session_id, OwnershipRevocationReason::SessionExpired);
        }
        let closed_spaces: Vec<String> = self
            .spaces
            .iter()
            .filter(|(_, space)| space.close_deadline.is_some_and(|deadline| deadline <= now))
            .map(|(space_key, _)| space_key.clone())
            .collect();
        for space_key in closed_spaces {
            if self
                .participants
                .values()
                .any(|participant| participant.spec.space_key == space_key)
            {
                continue;
            }
            let Some(space) = self.spaces.remove(&space_key) else {
                continue;
            };
            self.runtime
                .destroy_shard(space.handle.shard(), "empty Space grace elapsed");
            self.broadcast_space_closed(&space_key, &space);
        }
    }

    fn remove_session(&mut self, session_id: SessionId, reason: OwnershipRevocationReason) {
        let participants: Vec<Participant> = self
            .participants
            .values()
            .filter(|participant| participant.owner == session_id)
            .cloned()
            .collect();
        for participant in participants {
            self.release(
                session_id,
                &participant.participant_id,
                &participant.registration_id,
                &participant.ownership_token,
                reason,
                None,
            );
        }
        if let Some(session) = self.sessions.remove(&session_id) {
            self.resume_tokens.remove(&session.resume_token);
            if let Some(stream_id) = session.stream_id {
                self.streams.remove(&stream_id);
            }
        }
    }

    fn send_grant(
        &mut self,
        session_id: SessionId,
        participant: &Participant,
        request_id: Vec<u8>,
    ) {
        self.send_session(
            session_id,
            ServerFrame {
                request_id,
                payload: Some(
                    crate::protocol::server_frame::Payload::ParticipantOwnershipGranted(
                        ParticipantOwnershipGranted {
                            participant_id: participant.participant_id.clone(),
                            registration_id: participant.registration_id.clone(),
                            ownership_token: participant.ownership_token.clone(),
                            client_spec_revision: participant.client_spec_revision,
                            accepted_spec_revision: participant.accepted_spec_revision,
                            applied_spec_revision: participant.applied_spec_revision,
                            published_generation: participant.published_generation,
                            mumble_join_token: participant.join_token.clone(),
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
                    crate::protocol::server_frame::Payload::ParticipantOwnershipRevoked(
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
        self.send_session(
            participant.owner,
            ServerFrame {
                request_id: Vec::new(),
                payload: Some(
                    crate::protocol::server_frame::Payload::ParticipantStatusChanged(
                        ParticipantStatusChanged {
                            participant_id: participant.participant_id,
                            status: Some(ParticipantStatus {
                                mumble_connected: participant.connection.is_some(),
                                applied_space_key: participant
                                    .applied_space_key
                                    .unwrap_or_default(),
                                self_mute: participant.self_mute,
                                self_deaf: participant.self_deaf,
                                accepted_spec_revision: participant.accepted_spec_revision,
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
            .filter(|participant| participant.spec.space_key == space_key)
            .map(|participant| SpaceParticipant {
                participant_id: participant.participant_id.clone(),
                display_name: participant.spec.display_name.clone(),
                server_mute: participant.spec.server_mute,
                server_deaf: participant.spec.server_deaf,
                mumble_connected: participant.connection.is_some(),
            })
            .collect();
        Some(SpaceSnapshot {
            space_key: space_key.to_owned(),
            incarnation_id: space.incarnation_id.clone(),
            space_revision: space.space_revision,
            participants,
            published_generation: space.published_generation,
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
                        payload: Some(crate::protocol::server_frame::Payload::SpaceSnapshot(
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
                    payload: Some(crate::protocol::server_frame::Payload::SpaceSnapshot(
                        snapshot.clone(),
                    )),
                },
            );
        }
    }

    fn broadcast_space_closed(&mut self, space_key: &str, space: &Space) {
        let sessions: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, session)| session.explicit_observations.contains(space_key))
            .map(|(session_id, _)| *session_id)
            .collect();
        for session_id in sessions {
            self.send_session(
                session_id,
                ServerFrame {
                    request_id: Vec::new(),
                    payload: Some(crate::protocol::server_frame::Payload::SpaceClosed(
                        SpaceClosed {
                            space_key: space_key.to_owned(),
                            incarnation_id: space.incarnation_id.clone(),
                            final_space_revision: space.space_revision,
                        },
                    )),
                },
            );
        }
    }

    fn session_observes(&self, session_id: SessionId, space_key: &str) -> bool {
        self.sessions
            .get(&session_id)
            .is_some_and(|session| session.explicit_observations.contains(space_key))
            || self.participants.values().any(|participant| {
                participant.owner == session_id && participant.spec.space_key == space_key
            })
    }

    fn send_session(&mut self, session_id: SessionId, frame: ServerFrame) {
        if !frame.request_id.is_empty()
            && let Some(session) = self.sessions.get_mut(&session_id)
        {
            if !session.completed_requests.contains_key(&frame.request_id) {
                session.completed_order.push_back(frame.request_id.clone());
            }
            session
                .completed_requests
                .insert(frame.request_id.clone(), frame.clone());
            while session.completed_order.len() > self.config.queue_capacity {
                if let Some(expired) = session.completed_order.pop_front() {
                    session.completed_requests.remove(&expired);
                }
            }
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
                if let Some(session) = self.sessions.get_mut(&session_id) {
                    session.needs_resync = true;
                }
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
        payload: Some(crate::protocol::server_frame::Payload::CommandRejected(
            CommandRejected {
                code: code.into(),
                message: message.to_owned(),
            },
        )),
    }
}

fn validate_space_key(key: &str) -> Result<(), String> {
    if key.is_empty() || key.len() > 128 {
        return Err("space_key must contain between 1 and 128 bytes".to_owned());
    }
    Ok(())
}

fn validate_spec(spec: &ParticipantSpec) -> Result<(), String> {
    validate_space_key(&spec.space_key)?;
    if spec.display_name.is_empty() || spec.display_name.chars().count() > 64 {
        return Err("display_name must contain between 1 and 64 characters".to_owned());
    }
    Ok(())
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
    #[error("Controller session {0} disappeared before SessionReady was built")]
    MissingSession(u64),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use mumble_server_runtime_gateway::Runtime;
    use tokio::sync::mpsc;

    use super::*;
    use crate::protocol::client_frame::Payload as ClientPayload;
    use crate::protocol::server_frame::Payload as ServerPayload;
    use crate::protocol::{
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

    fn spec(space: &str, name: &str) -> ParticipantSpec {
        ParticipantSpec {
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
        participant_spec: ParticipantSpec,
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
        assert_eq!(grant.mumble_join_token.len(), 43);
        assert_ne!(grant.ownership_token, grant.mumble_join_token.as_bytes());

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
        assert_eq!(restored.mumble_join_token, grant.mumble_join_token);
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
                credential: Some(grant.mumble_join_token),
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
        assert_ne!(new_grant.mumble_join_token, old_grant.mumble_join_token);
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
                credential: Some(new_grant.mumble_join_token),
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
                credential: Some(grant.mumble_join_token),
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
        let actor = ControllerActor {
            config: ControllerConfig::default(),
            runtime: runtime.handle(),
            sender,
            control_epoch: vec![0; 16],
            sessions: HashMap::new(),
            resume_tokens: HashMap::new(),
            streams: HashMap::new(),
            participants: BTreeMap::new(),
            join_tokens: HashMap::new(),
            spaces: BTreeMap::new(),
            next_session_id: 1,
            next_application_revision: 1,
        };
        (actor, receiver)
    }

    fn attached_session(
        actor: &mut ControllerActor,
        capacity: usize,
    ) -> mpsc::Receiver<Result<ServerFrame, Status>> {
        let (responses, receiver) = mpsc::channel(capacity);
        actor.sessions.insert(
            1,
            ControllerSession {
                controller_id: "controller".to_owned(),
                instance_id: vec![9],
                session_token: vec![7; 32],
                resume_token: vec![8; 32],
                expires_at: Instant::now() + Duration::from_secs(30),
                stream_id: Some(5),
                responses: Some(responses),
                desired_revision: 3,
                observed_revision: 0,
                explicit_observations: BTreeSet::new(),
                completed_requests: HashMap::new(),
                completed_order: VecDeque::new(),
                needs_resync: false,
            },
        );
        actor.streams.insert(5, 1);
        receiver
    }

    fn owned_participant(space_key: &str) -> Participant {
        Participant {
            participant_id: "alice".to_owned(),
            owner: 1,
            registration_id: vec![7],
            ownership_token: vec![1; 32],
            join_token: "join".to_owned(),
            client_spec_revision: 1,
            accepted_spec_revision: 1,
            applied_spec_revision: 0,
            applied_space_key: None,
            published_generation: 0,
            spec: spec(space_key, "Alice"),
            connection: None,
            self_mute: false,
            self_deaf: false,
            application_error: String::new(),
        }
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
        actor
            .participants
            .insert("alice".to_owned(), owned_participant("lobby"));

        actor.refresh_space("lobby");
        let lobby_revision = actor.next_application_revision - 1;
        if let Some(participant) = actor.participants.get_mut("alice") {
            participant.spec.space_key = "arena".to_owned();
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
