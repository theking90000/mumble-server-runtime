//! The Aurora/Borealis scenario end to end, against the composed server.
//!
//! This is where the live scenario belongs now: the runtime has no business
//! model, so only the composition can exercise one over real sockets.

#![allow(clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use voxloom_flavor::ServerPresentation;
use voxloom_flavor_reference::ReferenceFlavor;
use voxloom_protocol::ControlMessage;
use voxloom_protocol::messages::tcp;
use voxloom_server::config::ServerConfig;
use voxloom_server::server::{Server, ServerHandle};
use voxloom_server::tls::{self, Identity};
use voxloom_testkit::{ClientModel, SimulatedMumbleClient};

const NORMAL_TARGET: u32 = 0;
const DEADLINE: Duration = Duration::from_secs(5);

async fn start_server() -> ServerHandle {
    tls::install_crypto_provider();
    let identity = Identity::self_signed(vec!["localhost".to_string()]).expect("identity");
    let address: SocketAddr = "127.0.0.1:0".parse().expect("loopback address");
    let config = ServerConfig::default();
    let flavor = Arc::new(ReferenceFlavor::new(
        config.server_name.clone(),
        ServerPresentation::default(),
    ));
    Server::bind(config, flavor, identity, address, address)
        .await
        .expect("bind server")
        .spawn()
        .expect("spawn server")
}

fn channel_named(model: &ClientModel, fragment: &str) -> u32 {
    model
        .channels
        .values()
        .find(|channel| channel.name.contains(fragment))
        .map(|channel| channel.id)
        .unwrap_or_else(|| panic!("no channel containing {fragment:?}"))
}

/// Two members of one realm keep hearing each other while one of them joins a
/// channel, and neither is ever sent its own speech back.
#[tokio::test(flavor = "multi_thread")]
async fn a_channel_join_keeps_the_audio_flowing_and_never_echoes() {
    let handle = start_server().await;

    let mut alice = SimulatedMumbleClient::connect(handle.tcp_addr, "alice@aurora")
        .await
        .expect("alice connects");
    alice.drive_handshake().await.expect("alice syncs");
    let mut bob = SimulatedMumbleClient::connect(handle.tcp_addr, "bob@borealis")
        .await
        .expect("bob connects");
    bob.drive_handshake().await.expect("bob syncs");

    alice
        .associate_udp(handle.udp_addr, DEADLINE)
        .await
        .expect("alice udp");
    bob.associate_udp(handle.udp_addr, DEADLINE)
        .await
        .expect("bob udp");

    // Alice joins Bob's realm, exactly as clicking the channel does.
    let target = channel_named(alice.model(), "Borealis");
    alice
        .send_control(&ControlMessage::UserState(tcp::UserState {
            session: alice.self_session(),
            channel_id: Some(target),
            ..Default::default()
        }))
        .await
        .expect("alice asks to join");

    let bob_session = bob.self_session().expect("bob session");
    alice
        .wait_until(DEADLINE, |model| model.users.contains_key(&bob_session))
        .await
        .expect("alice sees bob after the join");
    let alice_session = alice.self_session().expect("alice session");
    bob.wait_until(DEADLINE, |model| model.users.contains_key(&alice_session))
        .await
        .expect("bob sees alice after the join");

    // Speech keeps flowing well past the 5 s mark, which is where a real client
    // reported losing it.
    for frame in 0..40u64 {
        alice
            .speak(handle.udp_addr, NORMAL_TARGET, frame, &[0xAA, 0xBB, 0xCC])
            .await
            .expect("alice speaks");

        let heard = bob
            .recv_voice(Duration::from_millis(500))
            .await
            .expect("bob receives");
        match heard {
            Some(audio) => assert_eq!(
                audio.sender_session, alice_session,
                "frame {frame} came from the wrong session"
            ),
            None => panic!("bob stopped hearing alice at frame {frame}"),
        }

        // Alice must never be sent her own speech.
        if let Some(echo) = alice
            .recv_voice(Duration::from_millis(20))
            .await
            .expect("alice receives")
        {
            panic!(
                "alice was sent audio from session {} at frame {frame}",
                echo.sender_session
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    handle.shutdown();
}

/// A session that behaves like the official client does: a self-state right
/// after sync, a TCP ping every 5 s, a UDP ping every 5 s, and continuous
/// speech. The reported symptom is audio dying about 5 s after a channel join,
/// which is exactly one ping interval.
#[tokio::test(flavor = "multi_thread")]
async fn audio_survives_a_ping_interval_after_a_join() {
    let handle = start_server().await;

    let mut alice = SimulatedMumbleClient::connect(handle.tcp_addr, "alice@aurora")
        .await
        .expect("alice connects");
    alice.drive_handshake().await.expect("alice syncs");
    let mut bob = SimulatedMumbleClient::connect(handle.tcp_addr, "bob@borealis")
        .await
        .expect("bob connects");
    bob.drive_handshake().await.expect("bob syncs");
    alice
        .associate_udp(handle.udp_addr, DEADLINE)
        .await
        .expect("alice udp");
    bob.associate_udp(handle.udp_addr, DEADLINE)
        .await
        .expect("bob udp");

    // What every real client sends right after ServerSync.
    for client in [&mut alice, &mut bob] {
        client
            .send_control(&ControlMessage::UserState(tcp::UserState {
                session: client.self_session(),
                self_mute: Some(false),
                self_deaf: Some(false),
                ..Default::default()
            }))
            .await
            .expect("self state");
    }

    let target = channel_named(alice.model(), "Borealis");
    alice
        .send_control(&ControlMessage::UserState(tcp::UserState {
            session: alice.self_session(),
            channel_id: Some(target),
            ..Default::default()
        }))
        .await
        .expect("join");
    let alice_session = alice.self_session().expect("alice session");
    bob.wait_until(DEADLINE, |model| model.users.contains_key(&alice_session))
        .await
        .expect("bob sees alice");

    // Ten seconds of speech, crossing two ping intervals.
    let start = tokio::time::Instant::now();
    let mut frame = 0u64;
    let mut heard = 0u64;
    let mut pings = 0u64;
    while start.elapsed() < Duration::from_secs(10) {
        if start.elapsed().as_secs() >= pings * 5 {
            pings += 1;
            alice
                .send_control(&ControlMessage::Ping(tcp::Ping {
                    timestamp: Some(pings),
                    ..Default::default()
                }))
                .await
                .expect("tcp ping");
            alice
                .associate_udp(handle.udp_addr, DEADLINE)
                .await
                .expect("udp ping");
        }

        frame += 1;
        alice
            .speak(handle.udp_addr, NORMAL_TARGET, frame, &[0xAA, 0xBB, 0xCC])
            .await
            .expect("alice speaks");
        if bob
            .recv_voice(Duration::from_millis(200))
            .await
            .expect("bob receives")
            .is_some()
        {
            heard += 1;
        }
        // A frame every 20 ms, as a 20 ms Opus framing does.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    eprintln!(
        "frames sent={frame} heard={heard} over {:?}",
        start.elapsed()
    );
    assert!(
        heard * 10 >= frame * 9,
        "bob lost {} of {frame} frames; audio dies during the run",
        frame - heard
    );

    handle.shutdown();
}

/// Target 31 is the client-requested server loopback: the server sends the
/// speaker its own audio back. It bypasses the flavor entirely, which is what
/// an "internal echo the client settings cannot turn off" looks like.
#[tokio::test(flavor = "multi_thread")]
async fn the_server_loopback_target_echoes_the_speaker() {
    let handle = start_server().await;
    let mut alice = SimulatedMumbleClient::connect(handle.tcp_addr, "alice@aurora")
        .await
        .expect("alice connects");
    alice.drive_handshake().await.expect("alice syncs");
    alice
        .associate_udp(handle.udp_addr, DEADLINE)
        .await
        .expect("alice udp");

    alice
        .speak(handle.udp_addr, 31, 1, &[0xAA, 0xBB, 0xCC])
        .await
        .expect("alice speaks to the loopback target");

    let echoed = alice
        .recv_voice(Duration::from_millis(500))
        .await
        .expect("alice receives");
    eprintln!("LOOPBACK ECHO: {echoed:?}");

    // Normal speech, by contrast, never comes back to a lone speaker.
    alice
        .speak(handle.udp_addr, NORMAL_TARGET, 2, &[0xAA, 0xBB, 0xCC])
        .await
        .expect("alice speaks normally");
    let normal = alice
        .recv_voice(Duration::from_millis(300))
        .await
        .expect("alice receives");
    eprintln!("NORMAL ECHO: {normal:?}");
    handle.shutdown();
}
