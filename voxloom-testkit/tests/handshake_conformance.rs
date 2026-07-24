//! Phase 3 conformance: the `SimulatedMumbleClient` (the judge) replays the
//! server's handshake and asserts it violates no §20 invariant, and two clients
//! connect simultaneously with independent in-memory views.
//!
//! These are the machine-verifiable P3 done criteria. The remaining criterion —
//! a real official client connecting and hearing loopback — is a human checkpoint
//! (docs/checklists/p3-minimal-server.md).
#![allow(clippy::expect_used)]

use std::net::SocketAddr;
use std::time::Duration;

use voxloom_server::config::ServerConfig;
use voxloom_server::server::{Server, ServerHandle};
use voxloom_server::tls::{self, Identity};
use voxloom_testkit::SimulatedMumbleClient;

/// Start a Phase 3 server on ephemeral 127.0.0.1 ports.
async fn start_server() -> ServerHandle {
    tls::install_crypto_provider();
    let identity = Identity::self_signed(vec!["localhost".to_string()]).expect("identity");
    let addr: SocketAddr = "127.0.0.1:0".parse().expect("addr");
    Server::bind(ServerConfig::default(), identity, addr, addr)
        .await
        .expect("bind")
        .spawn()
        .expect("spawn")
}

#[tokio::test]
async fn simulated_client_replays_handshake_without_violation() {
    let handle = start_server().await;

    let mut client = SimulatedMumbleClient::connect(handle.tcp_addr, "alice")
        .await
        .expect("connect");
    // Panics inside the model if any §20 invariant is violated during sync.
    client.drive_handshake().await.expect("handshake");

    let model = client.model();
    assert!(model.synced, "client must reach ServerSync");
    let session = model.self_session.expect("self session assigned");
    assert!(
        model.users.contains_key(&session),
        "the self-user must be present in the view (§20 invariant 6)"
    );
    assert!(
        model.channels.contains_key(&0),
        "the root channel (0) must exist (§20 invariant 1)"
    );

    handle.shutdown();
}

#[tokio::test]
async fn two_clients_connected_simultaneously_have_independent_views() {
    let handle = start_server().await;

    // Alice connects and syncs first.
    let mut alice = SimulatedMumbleClient::connect(handle.tcp_addr, "alice")
        .await
        .expect("alice connect");
    alice.drive_handshake().await.expect("alice handshake");

    // Bob connects while Alice is still connected; his handshake includes Alice
    // as an already-present user, validated by his own model.
    let mut bob = SimulatedMumbleClient::connect(handle.tcp_addr, "bob")
        .await
        .expect("bob connect");
    bob.drive_handshake().await.expect("bob handshake");

    // Alice picks up the presence broadcast announcing Bob.
    alice
        .pump(Duration::from_secs(2))
        .await
        .expect("alice pump");

    let alice_session = alice.self_session().expect("alice session");
    let bob_session = bob.self_session().expect("bob session");
    assert_ne!(
        alice_session, bob_session,
        "sessions must be unique across connections (§9.1)"
    );

    // Independent models, each a coherent view containing both users.
    let alice_view = alice.model();
    let bob_view = bob.model();

    assert!(alice_view.users.contains_key(&alice_session));
    assert!(
        alice_view.users.contains_key(&bob_session),
        "Alice should see Bob after his presence broadcast"
    );
    assert!(bob_view.users.contains_key(&bob_session));
    assert!(
        bob_view.users.contains_key(&alice_session),
        "Bob should see Alice (sent as an existing user during his sync)"
    );

    // The views are independent instances: each reports its own self-session.
    assert_eq!(alice_view.self_session, Some(alice_session));
    assert_eq!(bob_view.self_session, Some(bob_session));

    handle.shutdown();
}
