//! The Phase 4 machine criterion: N simulated clients, sustained traffic, zero
//! internal loss and bounded router latency.
//!
//! "Internal loss" is the point of the test. These sockets are on loopback, so a
//! packet that never arrives was not lost by a network: it was dropped by the
//! server, or routed to nobody, or silently swallowed by a decision the router
//! made. Every packet a speaker sends is therefore accounted for at every
//! listener, by frame number, and any gap fails the run.
//!
//! The judge is the verifier side of R2: it speaks the wire only through
//! `voxloom-protocol` and checks the server against the specification, never
//! against the server's own behaviour.

use std::time::{Duration, Instant};

use voxloom_flavor::ServerPresentation;
use voxloom_flavor_reference::ReferenceFlavor;
use voxloom_server::config::ServerConfig;
use voxloom_server::server::{Server, ServerHandle};
use voxloom_server::tls::{self, Identity};
use voxloom_testkit::SimulatedMumbleClient;

/// Normal talking.
const NORMAL_TARGET: u32 = 0;

/// How many clients take part. Every one of them hears every other, so the
/// delivery count grows as N*(N-1) per round.
const CLIENTS: usize = 4;

/// Rounds of speech per client. `CLIENTS * ROUNDS` packets in, and
/// `CLIENTS * ROUNDS * (CLIENTS - 1)` deliveries expected out.
const ROUNDS: u64 = 25;

/// Per-delivery ceiling. Generous by two orders of magnitude against a loopback
/// relay, because the assertion worth making is "the router does not stall",
/// not "the CI machine was fast today". A regression that matters (a lock held
/// across the packet path, a per-packet rebuild of the routing table) blows
/// through this budget, a busy runner does not.
const LATENCY_BUDGET: Duration = Duration::from_millis(500);

/// How long a listener waits for a packet it is owed before declaring it lost.
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);

/// The reference flavor, composed with the runtime exactly as the composition
/// binary does it. The runtime has no business model of its own, so a test that
/// wants the Aurora/Borealis scenario has to bring it.
fn reference_flavor(config: &ServerConfig) -> std::sync::Arc<ReferenceFlavor> {
    std::sync::Arc::new(ReferenceFlavor::new(
        config.server_name.clone(),
        ServerPresentation {
            welcome_text: (!config.welcome_text.is_empty()).then(|| config.welcome_text.clone()),
            allow_html: config.allow_html,
            max_message_length: Some(config.message_length),
            recording_allowed: config.recording_allowed,
        },
    ))
}

async fn start_server() -> ServerHandle {
    tls::install_crypto_provider();
    let identity = Identity::self_signed(vec!["localhost".to_string()]).expect("identity");
    let addr: std::net::SocketAddr = "127.0.0.1:0".parse().expect("addr");
    let config = ServerConfig::default();
    let flavor = reference_flavor(&config);
    let server = Server::bind(config, flavor, identity, addr, addr)
        .await
        .expect("bind server");
    server.spawn().expect("spawn server")
}

#[tokio::test(flavor = "multi_thread")]
async fn n_clients_relay_voice_without_internal_loss() {
    let handle = start_server().await;

    let mut clients = Vec::new();
    for index in 0..CLIENTS {
        let mut client = SimulatedMumbleClient::connect(handle.tcp_addr, &format!("judge{index}"))
            .await
            .expect("connect");
        client.drive_handshake().await.expect("handshake");
        client
            .associate_udp(handle.udp_addr, DELIVERY_TIMEOUT)
            .await
            .expect("associate udp");
        clients.push(client);
    }

    // Wait for every model to hold all participants before any voice moves.
    for client in &mut clients {
        client
            .wait_until(DELIVERY_TIMEOUT, |model| model.users.len() == CLIENTS)
            .await
            .expect("complete participant view");
    }
    for (index, client) in clients.iter().enumerate() {
        assert_eq!(
            client.model().users.len(),
            CLIENTS,
            "client {index} does not see every participant"
        );
    }

    let sessions: Vec<u32> = clients
        .iter()
        .map(|client| client.self_session().expect("synced session"))
        .collect();

    let mut worst_latency = Duration::ZERO;
    let mut deliveries = 0u64;

    for round in 0..ROUNDS {
        for speaker in 0..CLIENTS {
            // The payload identifies its origin, so a packet delivered to the
            // wrong listener or attributed to the wrong sender is caught rather
            // than counted.
            let opus = vec![
                u8::try_from(speaker).unwrap_or(0),
                u8::try_from(round & 0xFF).unwrap_or(0),
                0xA5,
            ];
            let sent_at = Instant::now();
            clients[speaker]
                .speak(handle.udp_addr, NORMAL_TARGET, round, &opus)
                .await
                .expect("speak");

            for (listener, client) in clients.iter_mut().enumerate() {
                if listener == speaker {
                    continue;
                }
                let heard = client
                    .recv_voice(DELIVERY_TIMEOUT)
                    .await
                    .expect("receiving voice failed")
                    .unwrap_or_else(|| {
                        panic!(
                            "internal loss: round {round}, packet from client {speaker} never reached client {listener}"
                        )
                    });

                worst_latency = worst_latency.max(sent_at.elapsed());
                deliveries += 1;

                assert_eq!(
                    heard.opus_data, opus,
                    "round {round}: client {listener} got a payload it was not sent"
                );
                assert_eq!(
                    heard.sender_session, sessions[speaker],
                    "round {round}: client {listener} misattributed the sender"
                );
                assert_eq!(
                    heard.frame_number, round,
                    "round {round}: frame numbering did not survive the relay"
                );
            }
        }
    }

    assert_eq!(
        deliveries,
        ROUNDS * CLIENTS as u64 * (CLIENTS as u64 - 1),
        "not every expected delivery happened"
    );
    assert!(
        worst_latency <= LATENCY_BUDGET,
        "router latency {worst_latency:?} exceeded the {LATENCY_BUDGET:?} budget"
    );

    handle.shutdown();
}

/// A speaker never hears itself on normal speech, however many rounds it sends.
/// This is the one delivery rule a routing bug is most likely to break by
/// accident, and it is audible as an echo the moment it does.
#[tokio::test(flavor = "multi_thread")]
async fn a_speaker_is_never_relayed_its_own_normal_speech() {
    let handle = start_server().await;

    let mut alice = SimulatedMumbleClient::connect(handle.tcp_addr, "alice")
        .await
        .expect("connect");
    alice.drive_handshake().await.expect("handshake");
    alice
        .associate_udp(handle.udp_addr, DELIVERY_TIMEOUT)
        .await
        .expect("associate udp");

    let mut bob = SimulatedMumbleClient::connect(handle.tcp_addr, "bob")
        .await
        .expect("connect");
    bob.drive_handshake().await.expect("handshake");
    bob.associate_udp(handle.udp_addr, DELIVERY_TIMEOUT)
        .await
        .expect("associate udp");

    for round in 0..ROUNDS {
        alice
            .speak(handle.udp_addr, NORMAL_TARGET, round, &[0x5A])
            .await
            .expect("speak");

        // Bob receiving proves the server finished routing this packet, so if an
        // echo were coming it would already be on Alice's socket.
        bob.recv_voice(DELIVERY_TIMEOUT)
            .await
            .expect("receiving voice failed")
            .unwrap_or_else(|| panic!("round {round}: bob heard nothing"));

        let echo = alice
            .recv_voice(Duration::from_millis(50))
            .await
            .expect("receiving voice failed");
        assert!(
            echo.is_none(),
            "round {round}: the server echoed normal speech back to its sender"
        );
    }

    handle.shutdown();
}
