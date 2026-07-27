//! Phase 6 verifier scenarios for live per-connection views.

// Integration setup and verifier construction use explicit expectations so
// failures identify the exact protocol stage.
#![allow(clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::time::Duration;

use voxloom_flavor_reference::ReferenceFlavor;
use voxloom_protocol::ControlMessage;
use voxloom_protocol::messages::tcp;
use voxloom_reconcile::{AudioRoute, ChannelIdKind};
use voxloom_render::{
    ChannelId, ChannelKey, ClientView, PermissionBits, SemanticKey, ServerPresentation, SessionId,
    UserKey, ViewChannel, ViewUser,
};
use voxloom_server::config::ServerConfig;
use voxloom_server::outbound::{CAPACITY, ControlAdmission, OutboundQueue, TransitionRefused};
use voxloom_server::server::{Server, ServerHandle};
use voxloom_server::tls::{self, Identity};
use voxloom_session::{ConnectionView, EmittedStep};
use voxloom_testkit::{ClientModel, SimulatedMumbleClient};

const VIEW_DEADLINE: Duration = Duration::from_secs(5);
const AUDIO_DEADLINE: Duration = Duration::from_secs(5);
const SILENCE_OBSERVATION: Duration = Duration::from_millis(150);
const NORMAL_TARGET: u32 = 0;

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
    let address: SocketAddr = "127.0.0.1:0".parse().expect("loopback address");
    let config = ServerConfig::default();
    let flavor = reference_flavor(&config);
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
        .unwrap_or_else(|| panic!("model has no channel containing {fragment:?}"))
}

fn semantic_channels(model: &ClientModel) -> BTreeSet<(String, Option<String>)> {
    model
        .channels
        .values()
        .map(|channel| {
            let parent = channel
                .parent
                .and_then(|parent| model.channels.get(&parent))
                .map(|parent| parent.name.clone());
            (channel.name.clone(), parent)
        })
        .collect()
}

fn semantic_users(model: &ClientModel) -> BTreeSet<(String, String)> {
    model
        .users
        .values()
        .map(|user| {
            let channel = model
                .channels
                .get(&user.channel)
                .map(|channel| channel.name.clone())
                .unwrap_or_else(|| panic!("user {} has no visible channel", user.name));
            (user.name.clone(), channel)
        })
        .collect()
}

async fn expect_audio(
    listener: &mut SimulatedMumbleClient,
    expected_sender: u32,
    expected_payload: &[u8],
) {
    let audio = listener
        .recv_voice(AUDIO_DEADLINE)
        .await
        .expect("receive audio")
        .expect("authorized audio was not delivered");
    assert_eq!(audio.sender_session, expected_sender);
    assert_eq!(audio.opus_data, expected_payload);
}

async fn expect_silence(listener: &mut SimulatedMumbleClient) {
    let audio = listener
        .recv_voice(SILENCE_OBSERVATION)
        .await
        .expect("observe audio isolation");
    assert!(audio.is_none(), "forbidden audio crossed the view boundary");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_live_views_diverge_converge_and_revoke_audio_without_reconnecting() {
    let handle = start_server().await;

    let mut alice = SimulatedMumbleClient::connect(handle.tcp_addr, "alice@aurora")
        .await
        .expect("alice connect");
    alice.drive_handshake().await.expect("alice handshake");
    alice
        .associate_udp(handle.udp_addr, AUDIO_DEADLINE)
        .await
        .expect("alice UDP association");

    let mut bob = SimulatedMumbleClient::connect(handle.tcp_addr, "bob@borealis")
        .await
        .expect("bob connect");
    bob.drive_handshake().await.expect("bob handshake");
    bob.associate_udp(handle.udp_addr, AUDIO_DEADLINE)
        .await
        .expect("bob UDP association");

    let alice_session = alice.self_session().expect("alice session");
    let bob_session = bob.self_session().expect("bob session");

    assert_eq!(
        alice.model().users.keys().copied().collect::<Vec<_>>(),
        vec![alice_session]
    );
    assert_eq!(
        bob.model().users.keys().copied().collect::<Vec<_>>(),
        vec![bob_session]
    );
    assert_ne!(
        semantic_channels(alice.model()),
        semantic_channels(bob.model()),
        "viewers in different realms must start with divergent trees"
    );

    alice
        .speak(handle.udp_addr, NORMAL_TARGET, 1, b"isolated")
        .await
        .expect("alice speaks while isolated");
    expect_silence(&mut bob).await;

    let bob_aurora = channel_named(bob.model(), "Aurora");
    bob.send_control(&ControlMessage::UserState(tcp::UserState {
        session: Some(bob_session),
        channel_id: Some(bob_aurora),
        ..Default::default()
    }))
    .await
    .expect("bob moves to Aurora");

    alice
        .wait_until(VIEW_DEADLINE, |model| {
            model.users.contains_key(&bob_session)
        })
        .await
        .expect("alice sees bob");
    bob.wait_until(VIEW_DEADLINE, |model| {
        model.users.contains_key(&alice_session)
            && model.users.get(&bob_session).map(|user| user.channel) == Some(bob_aurora)
    })
    .await
    .expect("bob converges to Aurora");

    assert_eq!(
        semantic_channels(alice.model()),
        semantic_channels(bob.model()),
        "same-realm viewers must converge on the same semantic tree"
    );
    assert_eq!(
        semantic_users(alice.model()),
        semantic_users(bob.model()),
        "same-realm viewers must converge on the same visible users"
    );

    alice
        .speak(handle.udp_addr, NORMAL_TARGET, 2, b"alice audible")
        .await
        .expect("alice speaks after convergence");
    expect_audio(&mut bob, alice_session, b"alice audible").await;
    bob.speak(handle.udp_addr, NORMAL_TARGET, 3, b"bob audible")
        .await
        .expect("bob speaks after convergence");
    expect_audio(&mut alice, bob_session, b"bob audible").await;

    let bob_borealis = channel_named(bob.model(), "Borealis");
    bob.send_control(&ControlMessage::UserState(tcp::UserState {
        session: Some(bob_session),
        channel_id: Some(bob_borealis),
        ..Default::default()
    }))
    .await
    .expect("bob moves to Borealis");

    alice
        .wait_until(VIEW_DEADLINE, |model| {
            !model.users.contains_key(&bob_session)
        })
        .await
        .expect("alice no longer sees bob");
    bob.wait_until(VIEW_DEADLINE, |model| {
        !model.users.contains_key(&alice_session)
            && model.users.get(&bob_session).map(|user| user.channel) == Some(bob_borealis)
    })
    .await
    .expect("bob re-diverges into Borealis");

    assert_ne!(
        semantic_channels(alice.model()),
        semantic_channels(bob.model()),
        "moving realms must re-diverge the trees without reconnecting"
    );

    alice
        .speak(handle.udp_addr, NORMAL_TARGET, 4, b"revoked alice")
        .await
        .expect("alice speaks after revocation");
    expect_silence(&mut bob).await;
    bob.speak(handle.udp_addr, NORMAL_TARGET, 5, b"revoked bob")
        .await
        .expect("bob speaks after revocation");
    expect_silence(&mut alice).await;

    handle.shutdown();
}

fn verifier_view(
    connection: &mut ConnectionView,
    channel_names: &[&str],
) -> (ClientView, Vec<ChannelId>) {
    let mut view = ClientView::empty();
    let self_session = connection.self_session();
    view.users.insert(
        self_session,
        ViewUser {
            key: UserKey(SemanticKey::Static("verifier:self".to_string())),
            session: self_session,
            name: "verifier".to_string(),
            channel: ChannelId::ROOT,
            user_id: None,
            certificate_hash: None,
            mute: false,
            deaf: false,
            suppress: false,
            self_mute: false,
            self_deaf: false,
            priority_speaker: false,
            recording: false,
            comment: None,
            texture: None,
        },
    );

    let mut ids = Vec::new();
    for name in channel_names {
        let key = ChannelKey(SemanticKey::Static(format!("verifier:{name}")));
        let id = connection
            .ids_mut()
            .resolve(key.clone(), ChannelIdKind::Stable)
            .expect("allocate verifier channel id");
        view.channels.insert(
            id,
            ViewChannel {
                key,
                id,
                parent: ChannelId::ROOT,
                name: (*name).to_string(),
                description: None,
                position: 0,
                temporary: false,
                max_users: None,
                enter_restricted: false,
                can_enter: true,
                links: BTreeSet::new(),
            },
        );
        ids.push(id);
    }
    view.permissions = view
        .channels
        .keys()
        .copied()
        .map(|channel| (channel, PermissionBits(PermissionBits::TRAVERSE)))
        .collect::<BTreeMap<_, _>>();
    view.server_presentation = ServerPresentation::default();
    (view, ids)
}

fn messages(steps: Vec<EmittedStep>) -> Vec<ControlMessage> {
    steps
        .into_iter()
        .map(|step| match step {
            EmittedStep::Message(message) => message,
            EmittedStep::RouteChange { .. } => {
                panic!("queue verifier views must not contain audio routes")
            }
        })
        .collect()
}

#[tokio::test]
async fn refused_transition_is_atomic_and_retry_converges_to_latest_view() {
    let self_session = SessionId(41);
    let mut connection = ConnectionView::new(self_session);
    let mut model = ClientModel::new();
    let no_routes = BTreeSet::<AudioRoute>::new();

    let (initial, _) = verifier_view(&mut connection, &[]);
    let initial_pending = connection
        .prepare(&initial, &no_routes)
        .expect("valid initial view")
        .expect("initial transition");
    let (initial_steps, initial_token) = initial_pending.split();
    for message in messages(initial_steps) {
        model.apply(&message);
    }
    connection
        .commit(initial_token)
        .expect("commit initial view");
    let committed_before_refusal = connection.committed().clone();

    let (queue, mut receiver) = OutboundQueue::new(self_session.0);
    while queue.depth() < CAPACITY.saturating_sub(1) {
        assert_eq!(
            queue.push_control(ControlMessage::Ping(tcp::Ping::default())),
            ControlAdmission::Accepted
        );
    }
    let depth_before_refusal = queue.depth();

    let (intermediate, intermediate_ids) =
        verifier_view(&mut connection, &["intermediate-a", "intermediate-b"]);
    let intermediate_pending = connection
        .prepare(&intermediate, &no_routes)
        .expect("valid intermediate view")
        .expect("intermediate transition");
    let (intermediate_steps, _intermediate_token) = intermediate_pending.split();
    let refusal = queue
        .send_transition(messages(intermediate_steps))
        .expect_err("transition cannot fit the remaining queue slot");
    assert!(matches!(refusal, TransitionRefused::Congested { .. }));
    assert_eq!(queue.depth(), depth_before_refusal);
    assert_eq!(connection.committed(), &committed_before_refusal);

    let mut drained = 0usize;
    while let Ok(message) = receiver.try_recv() {
        assert!(
            matches!(message, ControlMessage::Ping(_)),
            "a refused transition queued a partial view mutation"
        );
        drained = drained.saturating_add(1);
    }
    assert_eq!(drained, depth_before_refusal);

    let (latest, latest_ids) = verifier_view(&mut connection, &["latest"]);
    let latest_pending = connection
        .prepare(&latest, &no_routes)
        .expect("valid latest view")
        .expect("latest transition");
    let (latest_steps, latest_token) = latest_pending.split();
    queue
        .send_transition(messages(latest_steps))
        .expect("drained queue accepts latest transition");
    connection.commit(latest_token).expect("commit latest view");

    while let Ok(message) = receiver.try_recv() {
        model.apply(&message);
    }

    assert_eq!(connection.committed(), &latest);
    for id in intermediate_ids {
        assert!(
            !model.channels.contains_key(&id.0),
            "the refused intermediate view leaked into the client model"
        );
    }
    for id in latest_ids {
        assert_eq!(
            model
                .channels
                .get(&id.0)
                .map(|channel| channel.name.as_str()),
            Some("latest")
        );
    }
    assert_eq!(
        model
            .users
            .get(&self_session.0)
            .map(|user| user.name.as_str()),
        Some("verifier")
    );
}
