#![allow(clippy::expect_used)]

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use mumble_controller_core_conformance::protocol::client_frame::Payload as ClientPayload;
use mumble_controller_core_conformance::protocol::controller_service_client::ControllerServiceClient;
use mumble_controller_core_conformance::protocol::server_frame::Payload as ServerPayload;
use mumble_controller_core_conformance::protocol::{
    ClientFrame, CommandErrorCode, DesiredStateSnapshot, OpenSession, ParticipantRegistration,
    ParticipantSpec, RegisterParticipant, ReleaseParticipant, RenewLease, ServerFrame,
    SetParticipantSpec,
};
use mumble_controller_server::{ControllerConfig, RunningControllerServer};
use mumble_server_runtime_gateway::tls::Identity;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

const RESPONSE_DEADLINE: Duration = Duration::from_secs(5);

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
        tokio::time::timeout(RESPONSE_DEADLINE, self.responses.message())
            .await
            .expect("Controller response deadline")
            .expect("read Controller response")
            .expect("Controller stream ended")
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
        observed_space_keys: Vec::new(),
    }
}

fn open_frame(
    request: &str,
    controller: &str,
    instance: &[u8],
    resume_token: Vec<u8>,
) -> ClientFrame {
    ClientFrame {
        request_id: request_id(request),
        payload: Some(ClientPayload::OpenSession(OpenSession {
            controller_id: controller.to_owned(),
            controller_instance_id: instance.to_vec(),
            resume_token,
            desired_state: Some(desired_state(1)),
        })),
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

fn registration(registration_id: &[u8], spec: ParticipantSpec) -> ParticipantRegistration {
    ParticipantRegistration {
        participant_id: "participant".to_owned(),
        registration_id: registration_id.to_vec(),
        ownership_token: Vec::new(),
        client_spec_revision: 1,
        spec: Some(spec),
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
        second_grant.mumble_join_token,
        first_grant.mumble_join_token
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
                spec: Some(participant("arena", "Second")),
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
                spec: Some(participant("lobby", "Second")),
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
