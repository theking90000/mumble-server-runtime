//! Integration test for the async UDP voice relay (Phase 2, tranche T4).
//!
//! Drives the real relay loop ([`run_udp`]) over loopback UDP sockets, with a
//! fake client on one side and a fake upstream server on the other, both holding
//! the OCB2 domains their real counterparts would. It proves the parts T3's pure
//! re-encryptor could not: that the socket wiring correlates a datagram's source
//! address to the right session and re-encrypts voice losslessly in *both*
//! directions across genuine sockets.
//!
//! A live Mumble client through the built binary is still the Phase 2 human
//! oracle (see docs/checklists/p2-proxy-oracle.md); this test is the machine gate
//! that the wiring is correct before a human ever launches it.

// Tests use expect() for terse failure sites; unwrap() stays banned (gate).
#![allow(clippy::expect_used)]

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use mumble_server_runtime_crypto::CryptState;
use mumble_server_runtime_mitm_proxy::{ProxySecrets, Registration, Registry, Session, run_udp};
use mumble_server_runtime_protocol::messages::tcp;
use mumble_server_runtime_protocol::messages::udp::{Audio, audio};
use mumble_server_runtime_protocol::{ControlMessage, UdpMessage, decode_udp, encode_udp};
use tokio::net::UdpSocket;
use tokio::time::timeout;

// Distinct byte patterns so a swapped field is caught, never masked by symmetry.
const SERVER_KEY: [u8; 16] = [0x51; 16];
const SERVER_NONCE: [u8; 16] = [0x52; 16]; // server encrypt IV (server->client)
const CLIENT_NONCE: [u8; 16] = [0x53; 16]; // server decrypt IV (client->server)

const PROXY_KEY: [u8; 16] = [0x61; 16];
const PROXY_ENCRYPT: [u8; 16] = [0x62; 16]; // proxy encrypt IV (proxy->client)
const PROXY_DECRYPT: [u8; 16] = [0x63; 16]; // proxy decrypt IV (client->proxy)

const RECV_TIMEOUT: Duration = Duration::from_secs(2);

/// A distinct voice frame per direction, so a mixed-up leg cannot pass.
fn voice(session: u32, frame: u64, opus: &[u8]) -> Vec<u8> {
    encode_udp(&UdpMessage::Audio(Audio {
        header: Some(audio::Header::Target(0)),
        sender_session: session,
        frame_number: frame,
        opus_data: opus.to_vec(),
        positional_data: vec![],
        volume_adjustment: 0.0,
        is_terminator: false,
    }))
}

/// A session with its cipher domains established, as the server's initial
/// CryptSetup would build them, registered under the loopback host. The caller
/// keeps the returned [`Registration`] alive for as long as the session must stay
/// discoverable (dropping it deregisters).
fn registered_session(registry: &Registry) -> (Arc<StdMutex<Session>>, Registration) {
    let mut session = Session::new(ProxySecrets::new(PROXY_KEY, PROXY_ENCRYPT, PROXY_DECRYPT));
    session
        .process_from_server(ControlMessage::CryptSetup(tcp::CryptSetup {
            key: Some(SERVER_KEY.to_vec()),
            server_nonce: Some(SERVER_NONCE.to_vec()),
            client_nonce: Some(CLIENT_NONCE.to_vec()),
        }))
        .expect("establish cipher domains");
    let session = Arc::new(StdMutex::new(session));
    let registration = registry.register(Ipv4Addr::LOCALHOST.into(), Arc::clone(&session));
    (session, registration)
}

#[tokio::test]
async fn live_udp_relay_correlates_and_reencrypts_both_directions() {
    let registry = Registry::new();
    let (_session, _registration) = registered_session(&registry);

    // Fake upstream "real server": the proxy forwards re-encrypted client voice
    // here and we reply with re-encrypted server voice.
    let upstream = UdpSocket::bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)))
        .await
        .expect("bind fake upstream");
    let upstream_addr = upstream.local_addr().expect("upstream addr");

    // The proxy's client-facing socket. run_udp receives on it and answers on it.
    let client_facing = Arc::new(
        UdpSocket::bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)))
            .await
            .expect("bind proxy client-facing"),
    );
    let proxy_addr = client_facing.local_addr().expect("proxy addr");

    // Run the real relay loop; it never returns on its own, so we do not await it.
    tokio::spawn(run_udp(Arc::clone(&client_facing), upstream_addr, registry));

    // The fake client encrypts toward the proxy exactly as the real client would
    // after setKey(PROXY_KEY, client_nonce = PROXY_DECRYPT, server_nonce =
    // PROXY_ENCRYPT): its encrypt IV is PROXY_DECRYPT, decrypt IV is PROXY_ENCRYPT.
    let client = UdpSocket::bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)))
        .await
        .expect("bind fake client");
    let mut client_crypt = CryptState::new(&PROXY_KEY, &PROXY_DECRYPT, &PROXY_ENCRYPT);
    // The fake server decrypts client->server with CLIENT_NONCE and encrypts
    // server->client with SERVER_NONCE (the domain the proxy's to_server mirrors).
    let mut server_crypt = CryptState::new(&SERVER_KEY, &SERVER_NONCE, &CLIENT_NONCE);

    // client -> proxy -> server.
    let sent_up = voice(7, 1, &[0xAA, 0xBB, 0xCC]);
    let on_wire = client_crypt.encrypt(&sent_up).expect("client encrypt");
    client
        .send_to(&on_wire, proxy_addr)
        .await
        .expect("client sends voice");

    let mut buffer = vec![0u8; 2048];
    let (read, from_proxy) = timeout(RECV_TIMEOUT, upstream.recv_from(&mut buffer))
        .await
        .expect("upstream receives within timeout")
        .expect("upstream recv_from");
    let delivered_up = server_crypt
        .decrypt(&buffer[..read])
        .expect("server decrypts re-encrypted client voice");
    assert_eq!(
        decode_udp(&delivered_up).expect("decode delivered up"),
        decode_udp(&sent_up).expect("decode sent up"),
        "client->server voice changed through the proxy",
    );

    // server -> proxy -> client, replying to the proxy's uplink source address.
    let sent_down = voice(9, 2, &[0x11, 0x22, 0x33, 0x44]);
    let on_wire = server_crypt.encrypt(&sent_down).expect("server encrypt");
    upstream
        .send_to(&on_wire, from_proxy)
        .await
        .expect("server sends voice");

    let (read, _from) = timeout(RECV_TIMEOUT, client.recv_from(&mut buffer))
        .await
        .expect("client receives within timeout")
        .expect("client recv_from");
    let delivered_down = client_crypt
        .decrypt(&buffer[..read])
        .expect("client decrypts re-encrypted server voice");
    assert_eq!(
        decode_udp(&delivered_down).expect("decode delivered down"),
        decode_udp(&sent_down).expect("decode sent down"),
        "server->client voice changed through the proxy",
    );
}

#[tokio::test]
async fn unencrypted_connectivity_ping_crosses_before_any_session() {
    // An empty registry: no session exists yet. A connectivity ping must still
    // reach the upstream verbatim, the way the real client probes UDP before the
    // voice cipher is in play.
    let registry = Registry::new();

    let upstream = UdpSocket::bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)))
        .await
        .expect("bind fake upstream");
    let upstream_addr = upstream.local_addr().expect("upstream addr");

    let client_facing = Arc::new(
        UdpSocket::bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)))
            .await
            .expect("bind proxy client-facing"),
    );
    let proxy_addr = client_facing.local_addr().expect("proxy addr");

    tokio::spawn(run_udp(Arc::clone(&client_facing), upstream_addr, registry));

    let client = UdpSocket::bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)))
        .await
        .expect("bind fake client");

    // 12-byte legacy connectivity ping: four zero bytes then a timestamp.
    let mut ping = vec![0u8, 0, 0, 0];
    ping.extend_from_slice(&0x0123_4567_89ab_cdefu64.to_be_bytes());
    client
        .send_to(&ping, proxy_addr)
        .await
        .expect("client sends ping");

    let mut buffer = vec![0u8; 64];
    let (read, _from) = timeout(RECV_TIMEOUT, upstream.recv_from(&mut buffer))
        .await
        .expect("upstream receives the ping within timeout")
        .expect("upstream recv_from");
    assert_eq!(
        &buffer[..read],
        &ping[..],
        "ping was not forwarded verbatim"
    );
}
