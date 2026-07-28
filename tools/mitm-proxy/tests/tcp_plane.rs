//! Integration test for the MITM control plane (Phase 2, tranche T2).
//!
//! Drives the framed relay in-process over `tokio::io::duplex` pipes and asserts:
//! - every non-`CryptSetup` message survives decode/re-encode unchanged;
//! - the server's initial `CryptSetup` is rewritten to the proxy's own key/nonces
//!   before reaching the client, so the client never sees the server key;
//! - nonce resyncs are absorbed at the proxy (never forwarded), an empty request
//!   is answered by the proxy, and the two derived OCB2 domains actually work
//!   (a packet the real server encrypts decrypts under the server-facing state,
//!   and a packet the client-facing state encrypts decrypts under the real
//!   client's state).

// Tests use expect() for terse failure sites; unwrap() stays banned (gate).
#![allow(clippy::expect_used)]

use std::sync::{Arc, Mutex as StdMutex};

use mumble_server_runtime_crypto::CryptState;
use mumble_server_runtime_mitm_proxy::{Origin, ProxySecrets, Session, pump_control};
use mumble_server_runtime_protocol::messages::tcp;
use mumble_server_runtime_protocol::{ControlMessage, decode_frame, encode_frame, parse_frame};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex as TokioMutex;

// Distinct byte patterns so a swapped field is caught, never masked by symmetry.
const SERVER_KEY: [u8; 16] = [0x51; 16];
const SERVER_NONCE: [u8; 16] = [0x52; 16]; // server encrypt IV (server->client)
const CLIENT_NONCE: [u8; 16] = [0x53; 16]; // server decrypt IV (client->server)
const SERVER_RESYNC: [u8; 16] = [0x54; 16]; // server's new encrypt IV after a resync

const PROXY_KEY: [u8; 16] = [0x61; 16];
const PROXY_ENCRYPT: [u8; 16] = [0x62; 16]; // proxy encrypt IV (proxy->client)
const PROXY_DECRYPT: [u8; 16] = [0x63; 16]; // proxy decrypt IV (client->proxy)

const CLIENT_RESYNC: [u8; 16] = [0x71; 16]; // client's new encrypt IV after a resync

fn proxy_secrets() -> ProxySecrets {
    ProxySecrets::new(PROXY_KEY, PROXY_ENCRYPT, PROXY_DECRYPT)
}

fn full_server_setup() -> ControlMessage {
    ControlMessage::CryptSetup(tcp::CryptSetup {
        key: Some(SERVER_KEY.to_vec()),
        server_nonce: Some(SERVER_NONCE.to_vec()),
        client_nonce: Some(CLIENT_NONCE.to_vec()),
    })
}

fn frame_bytes(messages: &[ControlMessage]) -> Vec<u8> {
    let mut out = Vec::new();
    for message in messages {
        encode_frame(message, &mut out).expect("encode frame");
    }
    out
}

fn parse_all(mut bytes: &[u8]) -> Vec<ControlMessage> {
    let mut messages = Vec::new();
    while let Some(frame) = parse_frame(bytes).expect("parse frame") {
        messages.push(decode_frame(&frame).expect("decode frame"));
        bytes = &bytes[frame.total_len()..];
    }
    messages
}

/// Run one direction of the relay to completion over in-memory pipes, returning
/// (bytes forwarded to the far side, bytes replied toward the sender).
async fn run_direction(
    session: &Arc<StdMutex<Session>>,
    origin: Origin,
    input: &[u8],
) -> (Vec<u8>, Vec<u8>) {
    let (mut feed, reader) = tokio::io::duplex(64 * 1024);
    feed.write_all(input).await.expect("feed input");
    drop(feed); // EOF so the relay stops after draining.

    let (forward_writer, mut forward_reader) = tokio::io::duplex(64 * 1024);
    let (back_writer, mut back_reader) = tokio::io::duplex(64 * 1024);
    let forward = Arc::new(TokioMutex::new(forward_writer));
    let back = Arc::new(TokioMutex::new(back_writer));

    pump_control(
        reader,
        Arc::clone(&forward),
        Arc::clone(&back),
        Arc::clone(session),
        origin,
    )
    .await
    .expect("pump control");

    drop(forward); // close writers so read_to_end returns.
    drop(back);

    let mut forwarded = Vec::new();
    forward_reader
        .read_to_end(&mut forwarded)
        .await
        .expect("read forwarded");
    let mut replied = Vec::new();
    back_reader
        .read_to_end(&mut replied)
        .await
        .expect("read replied");
    (forwarded, replied)
}

#[tokio::test]
async fn server_plane_rewrites_key_absorbs_resync_and_derives_usable_domains() {
    let session = Arc::new(StdMutex::new(Session::new(proxy_secrets())));

    let input = frame_bytes(&[
        ControlMessage::Version(tcp::Version {
            release: Some("srv".to_string()),
            ..Default::default()
        }),
        full_server_setup(),
        ControlMessage::ServerSync(tcp::ServerSync {
            session: Some(7),
            ..Default::default()
        }),
        // A lone server_nonce: the server resyncs its encrypt IV.
        ControlMessage::CryptSetup(tcp::CryptSetup {
            key: None,
            client_nonce: None,
            server_nonce: Some(SERVER_RESYNC.to_vec()),
        }),
    ]);

    let (forwarded, replied) = run_direction(&session, Origin::Server, &input).await;
    assert!(replied.is_empty(), "server plane should not reply here");

    let forwarded = parse_all(&forwarded);
    // Version, rewritten CryptSetup, ServerSync — the resync is absorbed (dropped).
    assert_eq!(forwarded.len(), 3, "resync must not be forwarded");
    assert_eq!(
        forwarded[0],
        ControlMessage::Version(tcp::Version {
            release: Some("srv".to_string()),
            ..Default::default()
        })
    );
    match &forwarded[1] {
        ControlMessage::CryptSetup(cs) => {
            assert_eq!(
                cs.key,
                Some(PROXY_KEY.to_vec()),
                "client gets the proxy key"
            );
            assert_eq!(cs.server_nonce, Some(PROXY_ENCRYPT.to_vec()));
            assert_eq!(cs.client_nonce, Some(PROXY_DECRYPT.to_vec()));
        }
        other => panic!("expected rewritten CryptSetup, got {other:?}"),
    }
    assert_eq!(
        forwarded[2],
        ControlMessage::ServerSync(tcp::ServerSync {
            session: Some(7),
            ..Default::default()
        })
    );

    let mut guard = session.lock().expect("session lock");
    let channels = guard.channels_mut().expect("domains established");

    // Server-facing domain: encrypt IV mirrors the server's client_nonce; decrypt
    // IV took the resync value.
    assert_eq!(channels.to_server.encrypt_iv(), CLIENT_NONCE);
    assert_eq!(channels.to_server.decrypt_iv(), SERVER_RESYNC);
    // Client-facing domain: the proxy's own secrets.
    assert_eq!(channels.to_client.encrypt_iv(), PROXY_ENCRYPT);
    assert_eq!(channels.to_client.decrypt_iv(), PROXY_DECRYPT);

    // The real server, post-resync, encrypts with SERVER_RESYNC and decrypts with
    // CLIENT_NONCE. A packet it sends must decrypt under the server-facing state.
    let mut real_server = CryptState::new(&SERVER_KEY, &SERVER_RESYNC, &CLIENT_NONCE);
    let plaintext = b"voice-frame-from-server";
    let on_wire = real_server.encrypt(plaintext).expect("server encrypt");
    let decrypted = channels.to_server.decrypt(&on_wire).expect("proxy decrypt");
    assert_eq!(decrypted, plaintext);

    // The real client runs setKey(PROXY_KEY, client_nonce=PROXY_DECRYPT,
    // server_nonce=PROXY_ENCRYPT): encrypt IV = PROXY_DECRYPT, decrypt IV =
    // PROXY_ENCRYPT. A packet the client-facing state emits must decrypt there.
    let mut real_client = CryptState::new(&PROXY_KEY, &PROXY_DECRYPT, &PROXY_ENCRYPT);
    let plaintext = b"voice-frame-to-client";
    let on_wire = channels
        .to_client
        .encrypt(plaintext)
        .expect("proxy encrypt");
    let decrypted = real_client.decrypt(&on_wire).expect("client decrypt");
    assert_eq!(decrypted, plaintext);
}

#[tokio::test]
async fn client_plane_absorbs_resync_and_answers_requests() {
    let session = Arc::new(StdMutex::new(Session::new(proxy_secrets())));
    // Establish the domains as the server's initial CryptSetup would.
    session
        .lock()
        .expect("session lock")
        .process_from_server(full_server_setup())
        .expect("establish domains");

    let input = frame_bytes(&[
        ControlMessage::Ping(tcp::Ping::default()),
        // Client resyncs its encrypt IV: absorbed, applied to the client domain.
        ControlMessage::CryptSetup(tcp::CryptSetup {
            key: None,
            server_nonce: None,
            client_nonce: Some(CLIENT_RESYNC.to_vec()),
        }),
        // Empty request: the proxy answers toward the client, as the server would.
        ControlMessage::CryptSetup(tcp::CryptSetup::default()),
    ]);

    let (forwarded, replied) = run_direction(&session, Origin::Client, &input).await;

    // Only the Ping crosses to the server; both CryptSetup shapes are absorbed.
    let forwarded = parse_all(&forwarded);
    assert_eq!(forwarded, vec![ControlMessage::Ping(tcp::Ping::default())]);

    // The empty request is answered toward the client with the proxy's client-
    // facing encrypt IV.
    let replied = parse_all(&replied);
    assert_eq!(
        replied,
        vec![ControlMessage::CryptSetup(tcp::CryptSetup {
            key: None,
            client_nonce: None,
            server_nonce: Some(PROXY_ENCRYPT.to_vec()),
        })]
    );

    // The client resync landed on the client-facing decrypt side.
    let guard = session.lock().expect("session lock");
    let channels = guard.channels().expect("domains established");
    assert_eq!(channels.to_client.decrypt_iv(), CLIENT_RESYNC);
}
