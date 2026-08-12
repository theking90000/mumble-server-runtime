#![allow(clippy::expect_used)]

use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mumble_controller_server::{ControllerConfig, RunningControllerServer};
use mumble_server_runtime_gateway::tls::Identity;
use mumble_server_runtime_protocol::ControlMessage;
use mumble_server_runtime_protocol::messages::tcp;
use mumble_server_runtime_testkit::SimulatedMumbleClient;
use tempfile::NamedTempFile;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

const DEADLINE: Duration = Duration::from_secs(20);
const JAVA_START_DEADLINE: Duration = Duration::from_secs(120);
const SILENCE_DEADLINE: Duration = Duration::from_millis(150);
const NORMAL_TARGET: u32 = 0;

struct JavaController {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl JavaController {
    async fn start(endpoint: String, token_file: &std::path::Path) -> Self {
        let repository_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .expect("Spaces host is four levels below the repository root");
        let controller_dir = repository_root.join("control-plane");
        let mut command = Command::new(controller_dir.join("gradlew"));
        command.current_dir(controller_dir).arg("--no-daemon");
        if let (Ok(java_home), Ok(java8_home)) =
            (std::env::var("JAVA_HOME"), std::env::var("JAVA8_HOME"))
        {
            command.arg(format!(
                "-Dorg.gradle.java.installations.paths={java_home},{java8_home}"
            ));
        }
        let mut child = command
            .arg(":implementations:spaces:sdk:controller-spaces:controllerInterop")
            .arg(format!("-PinteropEndpoint={endpoint}"))
            .arg(format!("-PinteropTokenFile={}", token_file.display()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start Java ControllerSession driver");
        let input = child.stdin.take().expect("Java stdin");
        let output = child.stdout.take().expect("Java stdout");
        let mut controller = Self {
            child,
            input,
            output: BufReader::new(output),
        };
        controller
            .wait_for_with_deadline("CONTROLLER_INTEROP_READY", JAVA_START_DEADLINE)
            .await;
        controller
    }

    async fn command(&mut self, command: &str, expected: &str) {
        self.input
            .write_all(format!("{command}\n").as_bytes())
            .await
            .expect("write Java interop command");
        self.input.flush().await.expect("flush Java command");
        self.wait_for(expected).await;
    }

    async fn wait_for(&mut self, expected: &str) {
        self.wait_for_with_deadline(expected, DEADLINE).await;
    }

    async fn wait_for_with_deadline(&mut self, expected: &str, deadline: Duration) {
        tokio::time::timeout(deadline, async {
            loop {
                let mut line = String::new();
                let read = self
                    .output
                    .read_line(&mut line)
                    .await
                    .expect("read Java driver output");
                assert_ne!(read, 0, "Java driver ended before {expected}");
                if line.trim() == expected {
                    return;
                }
            }
        })
        .await
        .expect("Java driver acknowledgement deadline");
    }

    async fn finish(mut self) {
        self.command("STOP", "CONTROLLER_INTEROP_STOPPED").await;
        let status = tokio::time::timeout(DEADLINE, self.child.wait())
            .await
            .expect("Java process exit deadline")
            .expect("wait for Java process");
        assert!(status.success(), "Java interop driver failed");
    }
}

/// Assert that the server closed the control connection.
///
/// `wait_until` returns `Err` on timeout as well, so `is_err()` alone proves
/// nothing: it holds just as well for a connection that stayed perfectly open.
/// Only the error that is *not* the timeout witnesses a close.
async fn assert_closed(client: &mut SimulatedMumbleClient, reason: &str) {
    let error = client
        .wait_until(DEADLINE, |_model| false)
        .await
        .expect_err(reason);
    let report = format!("{error:#}");
    assert!(
        !report.contains("timed out"),
        "{reason}: the connection was still open after {DEADLINE:?} ({report})"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "run through ci/controller-interop.sh with Java 8 and Java 17 configured"]
async fn real_java_session_drives_real_mumble_clients() {
    let identity =
        Identity::self_signed(vec!["localhost".to_owned()]).expect("development identity");
    let server = RunningControllerServer::start(
        ControllerConfig {
            controller_bind: "127.0.0.1:0".parse().expect("controller address"),
            mumble_bind: "127.0.0.1:0".parse().expect("Mumble address"),
            ..ControllerConfig::default()
        },
        identity,
    )
    .await
    .expect("start real Controller server");
    let token_file = NamedTempFile::new().expect("private token file");
    let endpoint = format!("http://{}", server.controller_address());
    let mut java = JavaController::start(endpoint, token_file.path()).await;
    let credentials = std::fs::read_to_string(token_file.path()).expect("read join-token channel");
    let mut credentials = credentials.lines();
    let alice_token = credentials.next().expect("Alice join token").to_owned();
    let bob_token = credentials.next().expect("Bob join token").to_owned();
    assert!(
        credentials.next().is_none(),
        "unexpected credential payload"
    );
    token_file.close().expect("remove join-token channel");

    let mut alice = SimulatedMumbleClient::connect_with_credential(
        server.mumble_address(),
        "ignored-alice-name",
        &alice_token,
    )
    .await
    .expect("Alice Mumble connect");
    alice.drive_handshake().await.expect("Alice handshake");
    alice
        .associate_udp(server.mumble_address(), DEADLINE)
        .await
        .expect("Alice UDP association");

    let mut bob = SimulatedMumbleClient::connect_with_credential(
        server.mumble_address(),
        "ignored-bob-name",
        &bob_token,
    )
    .await
    .expect("Bob Mumble connect");
    bob.drive_handshake().await.expect("Bob handshake");
    bob.associate_udp(server.mumble_address(), DEADLINE)
        .await
        .expect("Bob UDP association");
    let alice_session = alice.self_session().expect("Alice session");
    let bob_session = bob.self_session().expect("Bob session");
    alice
        .wait_until(DEADLINE, |model| model.users.contains_key(&bob_session))
        .await
        .expect("Alice sees Bob");
    assert_eq!(
        alice
            .model()
            .users
            .get(&alice_session)
            .map(|user| user.name.as_str()),
        Some("Alice"),
        "the Controller display name must override the client proposal"
    );

    alice
        .speak(server.mumble_address(), NORMAL_TARGET, 1, &[0xA1])
        .await
        .expect("Alice voice");
    assert!(
        bob.recv_voice(DEADLINE)
            .await
            .expect("Bob voice receive")
            .is_some()
    );

    java.command("MUTE", "CONTROLLER_INTEROP_MUTED").await;
    alice
        .speak(server.mumble_address(), NORMAL_TARGET, 2, &[0xA2])
        .await
        .expect("muted Alice voice");
    assert!(
        bob.recv_voice(SILENCE_DEADLINE)
            .await
            .expect("server-mute silence")
            .is_none()
    );
    java.command("UNMUTE", "CONTROLLER_INTEROP_UNMUTED").await;

    java.command("DEAF_BOB", "CONTROLLER_INTEROP_BOB_DEAF")
        .await;
    alice
        .speak(server.mumble_address(), NORMAL_TARGET, 3, &[0xA3])
        .await
        .expect("voice toward deaf Bob");
    assert!(
        bob.recv_voice(SILENCE_DEADLINE)
            .await
            .expect("server-deaf silence")
            .is_none()
    );
    java.command("UNDEAF_BOB", "CONTROLLER_INTEROP_BOB_UNDEAF")
        .await;

    alice
        .send_control(&ControlMessage::UserState(tcp::UserState {
            session: Some(alice_session),
            self_mute: Some(true),
            ..Default::default()
        }))
        .await
        .expect("request self mute");
    java.command("WAIT_SELF_MUTED", "CONTROLLER_INTEROP_SELF_MUTED")
        .await;

    let messages = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&messages);
    alice
        .send_control(&ControlMessage::TextMessage(tcp::TextMessage {
            channel_id: vec![0],
            message: "controller interop text".to_owned(),
            ..Default::default()
        }))
        .await
        .expect("send room text");
    bob.wait_until(DEADLINE, move |_model| {
        observed.fetch_add(1, Ordering::SeqCst) > 0
    })
    .await
    .expect("Bob receives the room text");

    let mut replacement = SimulatedMumbleClient::connect_with_credential(
        server.mumble_address(),
        "second-ignored-name",
        &alice_token,
    )
    .await
    .expect("replacement Alice connect");
    replacement
        .drive_handshake()
        .await
        .expect("replacement Alice handshake");
    let replacement_session = replacement.self_session().expect("replacement session");
    assert_ne!(replacement_session, alice_session);
    assert_closed(
        &mut alice,
        "the first connection must close after replacement",
    )
    .await;
    alice = replacement;

    java.command("MOVE", "CONTROLLER_INTEROP_MOVED").await;
    alice
        .wait_until(DEADLINE, |model| {
            model
                .channels
                .get(&0)
                .is_some_and(|channel| channel.name == "arena")
        })
        .await
        .expect("Alice migrates to arena");
    assert_eq!(alice.self_session(), Some(replacement_session));
    bob.wait_until(DEADLINE, |model| {
        !model.users.contains_key(&replacement_session)
    })
    .await
    .expect("Bob no longer sees migrated Alice");

    java.command("TRANSFER", "CONTROLLER_INTEROP_TRANSFERRED")
        .await;
    alice
        .wait_until(DEADLINE, |model| {
            model
                .channels
                .get(&0)
                .is_some_and(|channel| channel.name == "handoff")
        })
        .await
        .expect("ownership transfer migrates the existing connection");
    alice
        .send_control(&ControlMessage::Ping(tcp::Ping {
            timestamp: Some(42),
            ..Default::default()
        }))
        .await
        .expect("connection survives ownership transfer");
    assert_eq!(alice.self_session(), Some(replacement_session));

    java.command("RELEASE", "CONTROLLER_INTEROP_RELEASED").await;
    assert_closed(&mut alice, "release must close the participant connection").await;
    java.finish().await;
    server.shutdown().await;
}
