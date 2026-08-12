#![allow(clippy::expect_used)]

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use mumble_controller_core_conformance::protocol::client_frame::Payload as ClientPayload;
use mumble_controller_core_conformance::protocol::controller_service_client::ControllerServiceClient;
use mumble_controller_core_conformance::protocol::server_frame::Payload as ServerPayload;
use mumble_controller_core_conformance::protocol::{
    ClientFrame, CloseSession, CommandErrorCode, DesiredStateSnapshot, OpenSession,
    OwnershipRevocationReason, ParticipantOwnershipGranted, ParticipantRegistration,
    ProfilePayload, ProfileRef, RegisterParticipant, ReleaseParticipant, RenewLease, ServerFrame,
    SetParticipantSpec, SyncDesiredState,
};
use mumble_controller_core_conformance::spaces_protocol::{DesiredState, ParticipantSpec};
use mumble_controller_server::{ControllerConfig, RunningControllerServer};
use mumble_server_runtime_gateway::tls::Identity;
use prost::Message;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

const RESPONSE_DEADLINE: Duration = Duration::from_secs(5);

fn spaces_descriptor_digest() -> String {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let digest =
        manifest.join("../../../../implementations/spaces/protocol/controller-spaces-v1.pb.sha256");
    std::fs::read_to_string(digest)
        .expect("read the pinned Spaces descriptor digest")
        .trim()
        .to_owned()
}

struct ControllerStream {
    requests: mpsc::Sender<ClientFrame>,
    responses: tonic::Streaming<ServerFrame>,
}

impl ControllerStream {
    async fn connect(address: SocketAddr) -> Self {
        let endpoint = format!("http://{address}");
        let channel = tonic::transport::Endpoint::from_shared(endpoint)
            .expect("valid conformance endpoint")
            .connect()
            .await
            .expect("connect conformance client");
        let mut client = ControllerServiceClient::new(channel);
        let (requests, incoming) = mpsc::channel(32);
        let responses = client
            .connect(ReceiverStream::new(incoming))
            .await
            .expect("open Controller stream")
            .into_inner();
        Self {
            requests,
            responses,
        }
    }

    async fn send(&self, frame: ClientFrame) {
        self.requests
            .send(frame)
            .await
            .expect("send Controller frame");
    }

    async fn next(&mut self) -> ServerFrame {
        self.next_result()
            .await
            .expect("read Controller response")
            .expect("Controller stream ended")
    }

    async fn next_result(&mut self) -> Result<Option<ServerFrame>, tonic::Status> {
        tokio::time::timeout(RESPONSE_DEADLINE, self.responses.message())
            .await
            .expect("Controller response deadline")
    }

    async fn next_matching(&mut self, predicate: impl Fn(&ServerPayload) -> bool) -> ServerFrame {
        loop {
            let frame = self.next().await;
            if frame.payload.as_ref().is_some_and(&predicate) {
                return frame;
            }
        }
    }
}

async fn server(max_sessions: usize) -> RunningControllerServer {
    let identity = Identity::self_signed(vec!["localhost".to_owned()])
        .expect("development identity for conformance server");
    RunningControllerServer::start(
        ControllerConfig {
            controller_bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            mumble_bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            max_sessions,
            ..ControllerConfig::default()
        },
        identity,
    )
    .await
    .expect("start conformance server")
}

fn request_id(value: &str) -> Vec<u8> {
    value.as_bytes().to_vec()
}

fn desired_state(revision: u64) -> DesiredStateSnapshot {
    DesiredStateSnapshot {
        desired_state_revision: revision,
        participants: Vec::new(),
        profile_state: Some(ProfilePayload {
            protobuf: DesiredState::default().encode_to_vec(),
        }),
    }
}

// The verifier intentionally leaves future protocol fields at their defaults.
#[allow(clippy::field_reassign_with_default)]
fn open_frame(
    request: &str,
    controller: &str,
    instance: &[u8],
    resume_token: Vec<u8>,
) -> ClientFrame {
    let mut open = OpenSession::default();
    open.controller_id = controller.to_owned();
    open.controller_instance_id = instance.to_vec();
    open.resume_token = resume_token;
    open.desired_state = Some(desired_state(1));
    open.profile = Some(ProfileRef {
        profile_id: "mumble.controller.spaces".to_owned(),
        schema_version: 1,
        descriptor_digest: spaces_descriptor_digest(),
    });
    ClientFrame {
        request_id: request_id(request),
        payload: Some(ClientPayload::OpenSession(open)),
    }
}

async fn open(
    address: SocketAddr,
    controller: &str,
    instance: &[u8],
    resume_token: Vec<u8>,
) -> (
    ControllerStream,
    mumble_controller_core_conformance::protocol::SessionReady,
) {
    let mut stream = ControllerStream::connect(address).await;
    stream
        .send(open_frame("open", controller, instance, resume_token))
        .await;
    let ready = stream
        .next_matching(|payload| matches!(payload, ServerPayload::SessionReady(_)))
        .await;
    let Some(ServerPayload::SessionReady(ready)) = ready.payload else {
        panic!("matching response changed variant");
    };
    let reconciled = stream
        .next_matching(|payload| matches!(payload, ServerPayload::DesiredStateReconciled(_)))
        .await;
    assert_eq!(reconciled.request_id, request_id("open"));
    (stream, ready)
}

fn participant(space: &str, name: &str) -> ParticipantSpec {
    ParticipantSpec {
        space_key: space.to_owned(),
        display_name: name.to_owned(),
        server_mute: false,
        server_deaf: false,
    }
}

fn registration_for(
    participant_id: &str,
    registration_id: &[u8],
    ownership_token: Vec<u8>,
    client_spec_revision: u64,
    spec: ParticipantSpec,
) -> ParticipantRegistration {
    ParticipantRegistration {
        participant_id: participant_id.to_owned(),
        registration_id: registration_id.to_vec(),
        ownership_token,
        client_spec_revision,
        profile_spec: Some(ProfilePayload {
            protobuf: spec.encode_to_vec(),
        }),
    }
}

fn registration(registration_id: &[u8], spec: ParticipantSpec) -> ParticipantRegistration {
    registration_for("participant", registration_id, Vec::new(), 1, spec)
}

fn register_frame(
    request: &str,
    session_token: &[u8],
    registration: ParticipantRegistration,
) -> ClientFrame {
    ClientFrame {
        request_id: request_id(request),
        payload: Some(ClientPayload::RegisterParticipant(RegisterParticipant {
            session_token: session_token.to_vec(),
            participant: Some(registration),
        })),
    }
}

fn spec_frame(
    request: &str,
    session_token: &[u8],
    participant_id: &str,
    ownership_token: &[u8],
    client_spec_revision: u64,
    spec: ParticipantSpec,
) -> ClientFrame {
    ClientFrame {
        request_id: request_id(request),
        payload: Some(ClientPayload::SetParticipantSpec(SetParticipantSpec {
            session_token: session_token.to_vec(),
            participant_id: participant_id.to_owned(),
            ownership_token: ownership_token.to_vec(),
            client_spec_revision,
            profile_spec: Some(ProfilePayload {
                protobuf: spec.encode_to_vec(),
            }),
        })),
    }
}

async fn register(
    stream: &mut ControllerStream,
    request: &str,
    session_token: &[u8],
    registration: ParticipantRegistration,
) -> ParticipantOwnershipGranted {
    stream
        .send(register_frame(request, session_token, registration))
        .await;
    let frame = stream
        .next_matching(|payload| matches!(payload, ServerPayload::ParticipantOwnershipGranted(_)))
        .await;
    let Some(ServerPayload::ParticipantOwnershipGranted(grant)) = frame.payload else {
        panic!("matching response changed variant");
    };
    grant
}

async fn expect_accepted(stream: &mut ControllerStream) -> ServerFrame {
    stream
        .next_matching(|payload| matches!(payload, ServerPayload::ParticipantSpecAccepted(_)))
        .await
}

async fn expect_rejected(stream: &mut ControllerStream, expected: CommandErrorCode) -> ServerFrame {
    let frame = stream
        .next_matching(|payload| matches!(payload, ServerPayload::CommandRejected(_)))
        .await;
    let Some(ServerPayload::CommandRejected(rejection)) = frame.payload.as_ref() else {
        panic!("matching response changed variant");
    };
    assert_eq!(rejection.code, expected as i32);
    frame
}

#[derive(Debug, PartialEq, Eq)]
enum SpecOutcome {
    Accepted,
    Rejected(CommandErrorCode),
}

async fn next_spec_outcome(stream: &mut ControllerStream) -> SpecOutcome {
    let frame = stream
        .next_matching(|payload| {
            matches!(
                payload,
                ServerPayload::ParticipantSpecAccepted(_) | ServerPayload::CommandRejected(_)
            )
        })
        .await;
    match frame.payload {
        Some(ServerPayload::ParticipantSpecAccepted(_)) => SpecOutcome::Accepted,
        Some(ServerPayload::CommandRejected(rejection)) => {
            let code = CommandErrorCode::try_from(rejection.code)
                .expect("server returned a declared command error code");
            SpecOutcome::Rejected(code)
        }
        _ => panic!("matching response changed variant"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_rotates_only_the_session_capability() {
    let server = server(4).await;
    let (first, first_ready) = open(
        server.controller_address(),
        "resume-controller",
        b"stable-instance",
        Vec::new(),
    )
    .await;
    let resume_token = first_ready.resume_token.clone();
    drop(first);

    let (resumed, resumed_ready) = open(
        server.controller_address(),
        "resume-controller",
        b"stable-instance",
        resume_token.clone(),
    )
    .await;
    assert_eq!(resumed_ready.resume_token, resume_token);
    assert_ne!(resumed_ready.session_token, first_ready.session_token);
    assert_eq!(resumed_ready.control_epoch, first_ready.control_epoch);
    drop(resumed);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn takeover_fences_stale_commands_and_request_replays_are_idempotent() {
    let server = server(4).await;
    let (mut first, first_ready) = open(
        server.controller_address(),
        "first-controller",
        b"first-instance",
        Vec::new(),
    )
    .await;
    first
        .send(ClientFrame {
            request_id: request_id("first-register"),
            payload: Some(ClientPayload::RegisterParticipant(RegisterParticipant {
                session_token: first_ready.session_token.clone(),
                participant: Some(registration(
                    b"first-registration",
                    participant("lobby", "First"),
                )),
            })),
        })
        .await;
    let first_grant = first
        .next_matching(|payload| matches!(payload, ServerPayload::ParticipantOwnershipGranted(_)))
        .await;
    let Some(ServerPayload::ParticipantOwnershipGranted(first_grant)) = first_grant.payload else {
        panic!("matching response changed variant");
    };

    let (mut second, second_ready) = open(
        server.controller_address(),
        "second-controller",
        b"second-instance",
        Vec::new(),
    )
    .await;
    second
        .send(ClientFrame {
            request_id: request_id("second-register"),
            payload: Some(ClientPayload::RegisterParticipant(RegisterParticipant {
                session_token: second_ready.session_token.clone(),
                participant: Some(registration(
                    b"second-registration",
                    participant("lobby", "Second"),
                )),
            })),
        })
        .await;
    let second_grant = second
        .next_matching(|payload| matches!(payload, ServerPayload::ParticipantOwnershipGranted(_)))
        .await;
    let Some(ServerPayload::ParticipantOwnershipGranted(second_grant)) = second_grant.payload
    else {
        panic!("matching response changed variant");
    };
    assert_ne!(second_grant.ownership_token, first_grant.ownership_token);
    assert_ne!(
        second_grant.connection_credential,
        first_grant.connection_credential
    );

    first
        .send(ClientFrame {
            request_id: request_id("stale-release"),
            payload: Some(ClientPayload::ReleaseParticipant(ReleaseParticipant {
                session_token: first_ready.session_token,
                participant_id: "participant".to_owned(),
                registration_id: first_grant.registration_id,
                ownership_token: first_grant.ownership_token,
            })),
        })
        .await;
    let stale = first
        .next_matching(|payload| matches!(payload, ServerPayload::CommandRejected(_)))
        .await;
    let Some(ServerPayload::CommandRejected(stale)) = stale.payload else {
        panic!("matching response changed variant");
    };
    assert_eq!(stale.code, CommandErrorCode::OwnershipLost as i32);

    second
        .send(ClientFrame {
            request_id: request_id("revision-two"),
            payload: Some(ClientPayload::SetParticipantSpec(SetParticipantSpec {
                session_token: second_ready.session_token.clone(),
                participant_id: "participant".to_owned(),
                ownership_token: second_grant.ownership_token.clone(),
                client_spec_revision: 2,
                profile_spec: Some(ProfilePayload {
                    protobuf: participant("arena", "Second").encode_to_vec(),
                }),
            })),
        })
        .await;
    second
        .next_matching(|payload| matches!(payload, ServerPayload::ParticipantSpecAccepted(_)))
        .await;

    second
        .send(ClientFrame {
            request_id: request_id("stale-revision"),
            payload: Some(ClientPayload::SetParticipantSpec(SetParticipantSpec {
                session_token: second_ready.session_token.clone(),
                participant_id: "participant".to_owned(),
                ownership_token: second_grant.ownership_token.clone(),
                client_spec_revision: 1,
                profile_spec: Some(ProfilePayload {
                    protobuf: participant("lobby", "Second").encode_to_vec(),
                }),
            })),
        })
        .await;
    let stale_revision = second
        .next_matching(|payload| matches!(payload, ServerPayload::CommandRejected(_)))
        .await;
    let Some(ServerPayload::CommandRejected(stale_revision)) = stale_revision.payload else {
        panic!("matching response changed variant");
    };
    assert_eq!(stale_revision.code, CommandErrorCode::StaleRevision as i32);

    let release = ClientFrame {
        request_id: request_id("release"),
        payload: Some(ClientPayload::ReleaseParticipant(ReleaseParticipant {
            session_token: second_ready.session_token,
            participant_id: "participant".to_owned(),
            registration_id: second_grant.registration_id,
            ownership_token: second_grant.ownership_token,
        })),
    };
    second.send(release.clone()).await;
    let first_result = second
        .next_matching(|payload| matches!(payload, ServerPayload::ParticipantOwnershipRevoked(_)))
        .await;
    second.send(release).await;
    let replayed = second
        .next_matching(|payload| matches!(payload, ServerPayload::ParticipantOwnershipRevoked(_)))
        .await;
    assert_eq!(replayed, first_result);
    drop(first);
    drop(second);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lease_watermark_and_session_limits_fail_closed() {
    let server = server(1).await;
    let (mut first, ready) = open(
        server.controller_address(),
        "limited-controller",
        b"limited-instance",
        Vec::new(),
    )
    .await;
    first
        .send(ClientFrame {
            request_id: request_id("renew-stale"),
            payload: Some(ClientPayload::RenewLease(RenewLease {
                session_token: ready.session_token,
                desired_state_revision: 0,
            })),
        })
        .await;
    first
        .next_matching(|payload| matches!(payload, ServerPayload::ResyncRequired(_)))
        .await;

    let mut refused = ControllerStream::connect(server.controller_address()).await;
    refused
        .send(open_frame(
            "open-refused",
            "second-controller",
            b"second-instance",
            Vec::new(),
        ))
        .await;
    let refusal = refused
        .next_matching(|payload| matches!(payload, ServerPayload::CommandRejected(_)))
        .await;
    let Some(ServerPayload::CommandRejected(refusal)) = refusal.payload else {
        panic!("matching response changed variant");
    };
    assert_eq!(refusal.code, CommandErrorCode::ResourceExhausted as i32);
    drop(first);
    drop(refused);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_resumes_participant_and_rejects_the_rotated_session_token() {
    let server = server(4).await;
    let (mut first, first_ready) = open(
        server.controller_address(),
        "reconnect-controller",
        b"reconnect-instance",
        Vec::new(),
    )
    .await;
    let first_grant = register(
        &mut first,
        "register",
        &first_ready.session_token,
        registration(b"stable-registration", participant("lobby", "Before")),
    )
    .await;
    drop(first);

    let mut resumed = ControllerStream::connect(server.controller_address()).await;
    let mut frame = open_frame(
        "resume",
        "reconnect-controller",
        b"reconnect-instance",
        first_ready.resume_token.clone(),
    );
    let Some(ClientPayload::OpenSession(open)) = frame.payload.as_mut() else {
        panic!("open helper changed variant");
    };
    open.desired_state = Some(DesiredStateSnapshot {
        desired_state_revision: 2,
        participants: vec![registration_for(
            "participant",
            b"stable-registration",
            first_grant.ownership_token.clone(),
            1,
            participant("lobby", "Before"),
        )],
        profile_state: Some(ProfilePayload {
            protobuf: DesiredState::default().encode_to_vec(),
        }),
    });
    resumed.send(frame).await;
    let ready = resumed
        .next_matching(|payload| matches!(payload, ServerPayload::SessionReady(_)))
        .await;
    let Some(ServerPayload::SessionReady(resumed_ready)) = ready.payload else {
        panic!("matching response changed variant");
    };
    let grant = resumed
        .next_matching(|payload| matches!(payload, ServerPayload::ParticipantOwnershipGranted(_)))
        .await;
    let Some(ServerPayload::ParticipantOwnershipGranted(resumed_grant)) = grant.payload else {
        panic!("matching response changed variant");
    };
    resumed
        .next_matching(|payload| matches!(payload, ServerPayload::DesiredStateReconciled(_)))
        .await;

    assert_eq!(resumed_grant.ownership_token, first_grant.ownership_token);
    assert_eq!(
        resumed_grant.connection_credential,
        first_grant.connection_credential
    );
    assert_ne!(resumed_ready.session_token, first_ready.session_token);

    resumed
        .send(spec_frame(
            "rotated-token",
            &first_ready.session_token,
            "participant",
            &resumed_grant.ownership_token,
            2,
            participant("arena", "Stale session token"),
        ))
        .await;
    expect_rejected(&mut resumed, CommandErrorCode::SessionExpiredError).await;

    resumed
        .send(spec_frame(
            "current-token",
            &resumed_ready.session_token,
            "participant",
            &resumed_grant.ownership_token,
            2,
            participant("arena", "Resumed"),
        ))
        .await;
    expect_accepted(&mut resumed).await;
    drop(resumed);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn foreign_identity_cannot_resume_or_use_a_leaked_ownership_token() {
    let server = server(4).await;
    let (mut owner, owner_ready) = open(
        server.controller_address(),
        "owner-controller",
        b"owner-instance",
        Vec::new(),
    )
    .await;
    let grant = register(
        &mut owner,
        "owner-register",
        &owner_ready.session_token,
        registration(b"owner-registration", participant("lobby", "Owner")),
    )
    .await;

    let (mut foreign, foreign_ready) = open(
        server.controller_address(),
        "foreign-controller",
        b"foreign-instance",
        owner_ready.resume_token.clone(),
    )
    .await;
    assert_ne!(foreign_ready.resume_token, owner_ready.resume_token);
    assert_ne!(foreign_ready.session_token, owner_ready.session_token);

    foreign
        .send(spec_frame(
            "foreign-write",
            &foreign_ready.session_token,
            "participant",
            &grant.ownership_token,
            2,
            participant("arena", "Foreign"),
        ))
        .await;
    expect_rejected(&mut foreign, CommandErrorCode::OwnershipLost).await;

    owner
        .send(spec_frame(
            "owner-write",
            &owner_ready.session_token,
            "participant",
            &grant.ownership_token,
            2,
            participant("arena", "Owner retained"),
        ))
        .await;
    expect_accepted(&mut owner).await;
    drop(owner);
    drop(foreign);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_conflict_cannot_take_over_but_explicit_registration_can() {
    let server = server(4).await;
    let (mut first, first_ready) = open(
        server.controller_address(),
        "snapshot-owner",
        b"snapshot-owner-instance",
        Vec::new(),
    )
    .await;
    let first_grant = register(
        &mut first,
        "first-register",
        &first_ready.session_token,
        registration(b"first-registration", participant("lobby", "First")),
    )
    .await;
    let (mut second, second_ready) = open(
        server.controller_address(),
        "snapshot-contender",
        b"snapshot-contender-instance",
        Vec::new(),
    )
    .await;

    second
        .send(ClientFrame {
            request_id: request_id("conflicting-snapshot"),
            payload: Some(ClientPayload::SyncDesiredState(SyncDesiredState {
                session_token: second_ready.session_token.clone(),
                desired_state: Some(DesiredStateSnapshot {
                    desired_state_revision: 2,
                    participants: vec![registration_for(
                        "participant",
                        b"snapshot-registration",
                        Vec::new(),
                        1,
                        participant("arena", "Snapshot contender"),
                    )],
                    profile_state: Some(ProfilePayload {
                        protobuf: DesiredState::default().encode_to_vec(),
                    }),
                }),
            })),
        })
        .await;
    let revoked = second
        .next_matching(|payload| matches!(payload, ServerPayload::ParticipantOwnershipRevoked(_)))
        .await;
    let Some(ServerPayload::ParticipantOwnershipRevoked(revoked)) = revoked.payload else {
        panic!("matching response changed variant");
    };
    assert_eq!(
        revoked.reason,
        OwnershipRevocationReason::RegistrationRejected as i32
    );
    second
        .next_matching(|payload| matches!(payload, ServerPayload::DesiredStateReconciled(_)))
        .await;

    first
        .send(spec_frame(
            "owner-still-valid",
            &first_ready.session_token,
            "participant",
            &first_grant.ownership_token,
            2,
            participant("lobby", "Still first"),
        ))
        .await;
    expect_accepted(&mut first).await;

    let second_grant = register(
        &mut second,
        "explicit-takeover",
        &second_ready.session_token,
        registration(b"explicit-registration", participant("arena", "Second")),
    )
    .await;
    assert_ne!(second_grant.ownership_token, first_grant.ownership_token);
    drop(first);
    drop(second);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn conflicting_equal_and_older_revisions_leave_the_current_spec_writable() {
    let server = server(2).await;
    let (mut stream, ready) = open(
        server.controller_address(),
        "revision-controller",
        b"revision-instance",
        Vec::new(),
    )
    .await;
    let grant = register(
        &mut stream,
        "register",
        &ready.session_token,
        registration(b"revision-registration", participant("lobby", "One")),
    )
    .await;

    stream
        .send(spec_frame(
            "revision-two",
            &ready.session_token,
            "participant",
            &grant.ownership_token,
            2,
            participant("arena", "Two"),
        ))
        .await;
    expect_accepted(&mut stream).await;
    stream
        .send(spec_frame(
            "equal-conflict",
            &ready.session_token,
            "participant",
            &grant.ownership_token,
            2,
            participant("lobby", "Conflicting two"),
        ))
        .await;
    expect_rejected(&mut stream, CommandErrorCode::StaleRevision).await;
    stream
        .send(spec_frame(
            "older-conflict",
            &ready.session_token,
            "participant",
            &grant.ownership_token,
            1,
            participant("lobby", "Old"),
        ))
        .await;
    expect_rejected(&mut stream, CommandErrorCode::StaleRevision).await;
    stream
        .send(spec_frame(
            "revision-three",
            &ready.session_token,
            "participant",
            &grant.ownership_token,
            3,
            participant("arena", "Three"),
        ))
        .await;
    expect_accepted(&mut stream).await;
    drop(stream);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_request_id_with_different_payload_replays_without_mutation() {
    let server = server(2).await;
    let (mut stream, ready) = open(
        server.controller_address(),
        "dedup-controller",
        b"dedup-instance",
        Vec::new(),
    )
    .await;
    let grant = register(
        &mut stream,
        "register",
        &ready.session_token,
        registration(b"dedup-registration", participant("lobby", "One")),
    )
    .await;

    stream
        .send(spec_frame(
            "duplicate",
            &ready.session_token,
            "participant",
            &grant.ownership_token,
            2,
            participant("lobby", "Two"),
        ))
        .await;
    let original = expect_accepted(&mut stream).await;
    stream
        .send(spec_frame(
            "duplicate",
            &ready.session_token,
            "participant",
            &grant.ownership_token,
            3,
            participant("arena", "Ignored duplicate"),
        ))
        .await;
    let replay = expect_accepted(&mut stream).await;
    assert_eq!(replay, original);

    stream
        .send(spec_frame(
            "revision-three",
            &ready.session_token,
            "participant",
            &grant.ownership_token,
            3,
            participant("arena", "Applied once"),
        ))
        .await;
    expect_accepted(&mut stream).await;
    drop(stream);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn simultaneous_explicit_claims_converge_to_exactly_one_owner() {
    let server = server(4).await;
    let (mut first, first_ready) = open(
        server.controller_address(),
        "race-first",
        b"race-first-instance",
        Vec::new(),
    )
    .await;
    let (mut second, second_ready) = open(
        server.controller_address(),
        "race-second",
        b"race-second-instance",
        Vec::new(),
    )
    .await;
    let first_register = register_frame(
        "simultaneous-first",
        &first_ready.session_token,
        registration(b"simultaneous-first", participant("first", "First")),
    );
    let second_register = register_frame(
        "simultaneous-second",
        &second_ready.session_token,
        registration(b"simultaneous-second", participant("second", "Second")),
    );
    let ((), ()) = tokio::join!(first.send(first_register), second.send(second_register));
    let (first_grant_frame, second_grant_frame) = tokio::join!(
        first.next_matching(|payload| matches!(
            payload,
            ServerPayload::ParticipantOwnershipGranted(_)
        )),
        second.next_matching(|payload| matches!(
            payload,
            ServerPayload::ParticipantOwnershipGranted(_)
        ))
    );
    let Some(ServerPayload::ParticipantOwnershipGranted(first_grant)) = first_grant_frame.payload
    else {
        panic!("matching response changed variant");
    };
    let Some(ServerPayload::ParticipantOwnershipGranted(second_grant)) = second_grant_frame.payload
    else {
        panic!("matching response changed variant");
    };
    assert_ne!(first_grant.ownership_token, second_grant.ownership_token);

    let first_write = spec_frame(
        "first-race-write",
        &first_ready.session_token,
        "participant",
        &first_grant.ownership_token,
        2,
        participant("first", "First write"),
    );
    let second_write = spec_frame(
        "second-race-write",
        &second_ready.session_token,
        "participant",
        &second_grant.ownership_token,
        2,
        participant("second", "Second write"),
    );
    let ((), ()) = tokio::join!(first.send(first_write), second.send(second_write));
    let (first_outcome, second_outcome) = tokio::join!(
        next_spec_outcome(&mut first),
        next_spec_outcome(&mut second)
    );
    assert!(matches!(
        (&first_outcome, &second_outcome),
        (
            SpecOutcome::Accepted,
            SpecOutcome::Rejected(CommandErrorCode::OwnershipLost)
        ) | (
            SpecOutcome::Rejected(CommandErrorCode::OwnershipLost),
            SpecOutcome::Accepted
        )
    ));
    drop(first);
    drop(second);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn crossed_takeovers_of_two_participants_do_not_cross_fencing_tokens() {
    let server = server(4).await;
    let (mut first, first_ready) = open(
        server.controller_address(),
        "cross-first",
        b"cross-first-instance",
        Vec::new(),
    )
    .await;
    let (mut second, second_ready) = open(
        server.controller_address(),
        "cross-second",
        b"cross-second-instance",
        Vec::new(),
    )
    .await;
    let first_original = register(
        &mut first,
        "first-original",
        &first_ready.session_token,
        registration_for(
            "first-participant",
            b"first-original",
            Vec::new(),
            1,
            participant("lobby", "First original"),
        ),
    )
    .await;
    let second_original = register(
        &mut second,
        "second-original",
        &second_ready.session_token,
        registration_for(
            "second-participant",
            b"second-original",
            Vec::new(),
            1,
            participant("lobby", "Second original"),
        ),
    )
    .await;

    let first_takeover = register_frame(
        "first-takes-second",
        &first_ready.session_token,
        registration_for(
            "second-participant",
            b"first-takes-second",
            Vec::new(),
            1,
            participant("first-space", "First owns second"),
        ),
    );
    let second_takeover = register_frame(
        "second-takes-first",
        &second_ready.session_token,
        registration_for(
            "first-participant",
            b"second-takes-first",
            Vec::new(),
            1,
            participant("second-space", "Second owns first"),
        ),
    );
    let ((), ()) = tokio::join!(first.send(first_takeover), second.send(second_takeover));
    let (first_new_frame, second_new_frame) = tokio::join!(
        first.next_matching(|payload| matches!(
            payload,
            ServerPayload::ParticipantOwnershipGranted(grant)
                if grant.participant_id == "second-participant"
        )),
        second.next_matching(|payload| matches!(
            payload,
            ServerPayload::ParticipantOwnershipGranted(grant)
                if grant.participant_id == "first-participant"
        ))
    );
    let Some(ServerPayload::ParticipantOwnershipGranted(first_new)) = first_new_frame.payload
    else {
        panic!("matching response changed variant");
    };
    let Some(ServerPayload::ParticipantOwnershipGranted(second_new)) = second_new_frame.payload
    else {
        panic!("matching response changed variant");
    };

    first
        .send(spec_frame(
            "first-stale",
            &first_ready.session_token,
            "first-participant",
            &first_original.ownership_token,
            2,
            participant("lobby", "Stale first"),
        ))
        .await;
    expect_rejected(&mut first, CommandErrorCode::OwnershipLost).await;
    second
        .send(spec_frame(
            "second-stale",
            &second_ready.session_token,
            "second-participant",
            &second_original.ownership_token,
            2,
            participant("lobby", "Stale second"),
        ))
        .await;
    expect_rejected(&mut second, CommandErrorCode::OwnershipLost).await;
    first
        .send(spec_frame(
            "first-current",
            &first_ready.session_token,
            "second-participant",
            &first_new.ownership_token,
            2,
            participant("first-space", "Current first"),
        ))
        .await;
    expect_accepted(&mut first).await;
    second
        .send(spec_frame(
            "second-current",
            &second_ready.session_token,
            "first-participant",
            &second_new.ownership_token,
            2,
            participant("second-space", "Current second"),
        ))
        .await;
    expect_accepted(&mut second).await;
    drop(first);
    drop(second);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_the_replaced_session_cannot_release_the_new_owner() {
    let server = server(4).await;
    let (mut first, first_ready) = open(
        server.controller_address(),
        "close-first",
        b"close-first-instance",
        Vec::new(),
    )
    .await;
    register(
        &mut first,
        "first-register",
        &first_ready.session_token,
        registration(b"first-registration", participant("lobby", "First")),
    )
    .await;
    let (mut second, second_ready) = open(
        server.controller_address(),
        "close-second",
        b"close-second-instance",
        Vec::new(),
    )
    .await;
    let second_grant = register(
        &mut second,
        "second-register",
        &second_ready.session_token,
        registration(b"second-registration", participant("handoff", "Second")),
    )
    .await;

    first
        .send(ClientFrame {
            request_id: request_id("close-old-session"),
            payload: Some(ClientPayload::CloseSession(CloseSession {
                session_token: first_ready.session_token,
            })),
        })
        .await;
    first
        .next_matching(|payload| matches!(payload, ServerPayload::SessionClosing(_)))
        .await;

    second
        .send(spec_frame(
            "new-owner-write",
            &second_ready.session_token,
            "participant",
            &second_grant.ownership_token,
            2,
            participant("handoff", "Still second"),
        ))
        .await;
    expect_accepted(&mut second).await;
    drop(first);
    drop(second);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn identical_request_ids_on_different_sessions_do_not_alias() {
    let server = server(4).await;
    let (mut first, first_ready) = open(
        server.controller_address(),
        "request-first",
        b"request-first-instance",
        Vec::new(),
    )
    .await;
    let (mut second, second_ready) = open(
        server.controller_address(),
        "request-second",
        b"request-second-instance",
        Vec::new(),
    )
    .await;
    let first_frame = register_frame(
        "same-request-id",
        &first_ready.session_token,
        registration_for(
            "first-participant",
            b"first-registration",
            Vec::new(),
            1,
            participant("first", "First"),
        ),
    );
    let second_frame = register_frame(
        "same-request-id",
        &second_ready.session_token,
        registration_for(
            "second-participant",
            b"second-registration",
            Vec::new(),
            1,
            participant("second", "Second"),
        ),
    );
    let ((), ()) = tokio::join!(first.send(first_frame), second.send(second_frame));
    let (first_grant_frame, second_grant_frame) = tokio::join!(
        first.next_matching(|payload| matches!(
            payload,
            ServerPayload::ParticipantOwnershipGranted(_)
        )),
        second.next_matching(|payload| matches!(
            payload,
            ServerPayload::ParticipantOwnershipGranted(_)
        ))
    );
    let Some(ServerPayload::ParticipantOwnershipGranted(first_grant)) = first_grant_frame.payload
    else {
        panic!("matching response changed variant");
    };
    let Some(ServerPayload::ParticipantOwnershipGranted(second_grant)) = second_grant_frame.payload
    else {
        panic!("matching response changed variant");
    };
    assert_eq!(first_grant.participant_id, "first-participant");
    assert_eq!(second_grant.participant_id, "second-participant");
    assert_ne!(first_grant.ownership_token, second_grant.ownership_token);
    drop(first);
    drop(second);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn explicit_resume_of_the_same_registration_keeps_bearer_credentials() {
    let server = server(2).await;
    let (mut stream, ready) = open(
        server.controller_address(),
        "explicit-resume",
        b"explicit-resume-instance",
        Vec::new(),
    )
    .await;
    let first_grant = register(
        &mut stream,
        "first-register",
        &ready.session_token,
        registration(b"stable-registration", participant("lobby", "First")),
    )
    .await;
    let resumed_grant = register(
        &mut stream,
        "same-registration",
        &ready.session_token,
        registration_for(
            "participant",
            b"stable-registration",
            first_grant.ownership_token.clone(),
            2,
            participant("arena", "Resumed"),
        ),
    )
    .await;
    assert_eq!(resumed_grant.ownership_token, first_grant.ownership_token);
    assert_eq!(
        resumed_grant.connection_credential,
        first_grant.connection_credential
    );
    assert_eq!(resumed_grant.client_spec_revision, 2);
    drop(stream);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_malformed_registration_does_not_poison_a_valid_retry() {
    let server = server(2).await;
    let (mut stream, ready) = open(
        server.controller_address(),
        "malformed-controller",
        b"malformed-instance",
        Vec::new(),
    )
    .await;
    stream
        .send(register_frame(
            "malformed",
            &ready.session_token,
            ParticipantRegistration {
                participant_id: "participant".to_owned(),
                registration_id: b"malformed-registration".to_vec(),
                ownership_token: Vec::new(),
                client_spec_revision: 1,
                profile_spec: None,
            },
        ))
        .await;
    let failure = stream
        .next_result()
        .await
        .expect_err("malformed profile payload must terminate the invalid stream");
    assert_eq!(failure.code(), tonic::Code::InvalidArgument);
    drop(stream);

    let (mut resumed, resumed_ready) = open(
        server.controller_address(),
        "malformed-controller",
        b"malformed-instance",
        ready.resume_token,
    )
    .await;

    let grant = register(
        &mut resumed,
        "valid-retry",
        &resumed_ready.session_token,
        registration(b"valid-registration", participant("lobby", "Valid")),
    )
    .await;
    assert_eq!(grant.participant_id, "participant");
    drop(resumed);
    server.shutdown().await;
}
