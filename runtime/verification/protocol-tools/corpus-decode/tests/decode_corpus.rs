//! End-to-end decode of the real captured corpus. This is the Phase 1
//! done-criterion as an executable check: every scenario must decode with no
//! unexplained byte — every TCP stream fully framed, every UDP packet either
//! recognised as an unencrypted ping, OCB2-decrypted, or (never, for these
//! files) explicitly accounted as an OCB2 rejection.

use std::path::PathBuf;

use mumble_server_runtime_corpus_decode::{Decoded, UdpMessage, decode_session, read_records};

/// Scenarios captured against the local Mumble 1.5.857 server: their voice plane
/// is protobuf UDP, so they exercise `decode_udp`'s protobuf path on real traffic.
const PROTOBUF_SERVER_SCENARIOS: &[&str] = &[
    "03-channel-create-remove",
    "04-two-clients-talking",
    "05-whisper",
    "06-permission-denied",
    "07-disconnect",
];

/// Scenarios captured against a third-party Mumble 1.3.4 server: their voice plane
/// is the legacy wire format, which ADR-0001 keeps out of `mumble-server-runtime-protocol`.
const LEGACY_SERVER_SCENARIOS: &[&str] = &["01-handshake", "02-channel-join-leave"];

/// Count decoded protobuf Audio packets and decrypted legacy payloads in a
/// scenario. Audio is the discriminating signal on the voice plane: a 1.5 client
/// probes with a protobuf *ping* even against a 1.3.4 server, so ping presence
/// alone does not prove the server speaks protobuf — a decoded Audio packet does.
fn voice_plane_counts(scenario: &str) -> (usize, usize) {
    let path = corpus_dir().join(scenario).join("session.voxcap");
    let records =
        read_records(&path).unwrap_or_else(|error| panic!("reading {scenario}: {error:#}"));
    let transcript =
        decode_session(&records).unwrap_or_else(|error| panic!("decoding {scenario}: {error:#}"));

    let mut protobuf_audio = 0;
    let mut decrypted_legacy = 0;
    for event in &transcript.events {
        match &event.decoded {
            Decoded::Udp(message) if matches!(message.as_ref(), UdpMessage::Audio(_)) => {
                protobuf_audio += 1;
            }
            Decoded::DecryptedLegacyUdp { .. } => decrypted_legacy += 1,
            _ => {}
        }
    }
    (protobuf_audio, decrypted_legacy)
}

/// All corpus scenarios, addressed relative to this crate's manifest.
const SCENARIOS: &[&str] = &[
    "01-handshake",
    "02-channel-join-leave",
    "03-channel-create-remove",
    "04-two-clients-talking",
    "05-whisper",
    "06-permission-denied",
    "07-disconnect",
];

fn corpus_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is tools/corpus-decode; the corpus is at the root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/corpus")
}

#[test]
fn every_scenario_decodes_with_no_unexplained_byte() {
    let root = corpus_dir();
    for scenario in SCENARIOS {
        let path = root.join(scenario).join("session.voxcap");
        let records =
            read_records(&path).unwrap_or_else(|error| panic!("reading {scenario}: {error:#}"));
        assert!(!records.is_empty(), "{scenario}: empty capture");

        let transcript = decode_session(&records)
            .unwrap_or_else(|error| panic!("decoding {scenario}: {error:#}"));

        assert_eq!(
            transcript.tcp_trailing_c2s, 0,
            "{scenario}: {} trailing C2S TCP bytes (stream did not end on a frame boundary)",
            transcript.tcp_trailing_c2s
        );
        assert_eq!(
            transcript.tcp_trailing_s2c, 0,
            "{scenario}: {} trailing S2C TCP bytes",
            transcript.tcp_trailing_s2c
        );
        assert_eq!(
            transcript.udp_rejected, 0,
            "{scenario}: {} of {} UDP packets failed OCB2 decryption — a systemic \
             failure (e.g. a missed re-key), not a drop tail",
            transcript.udp_rejected, transcript.udp_packets
        );
        assert_eq!(
            transcript.events.len(),
            transcript.tcp_frames + transcript.udp_packets,
            "{scenario}: every frame and datagram must produce exactly one event"
        );
    }
}

/// Scenario 03 carries a mid-session full CryptSetup re-key. This locks in that
/// the decoder adopts the new key rather than latching the first one (which left
/// 347 of 383 packets undecryptable before the fix).
#[test]
fn rekey_scenario_decrypts_all_voice() {
    let path = corpus_dir().join("03-channel-create-remove/session.voxcap");
    let records = read_records(&path).expect("read 03");
    let transcript = decode_session(&records).expect("decode 03");
    assert_eq!(
        transcript.udp_rejected, 0,
        "re-key scenario must decrypt every packet after the new CryptSetup"
    );
    assert!(
        transcript.udp_packets > 300,
        "sanity: 03 should carry the voice packets that exercised the re-key"
    );
}

/// The corpus is mixed: 03-07 hit a 1.5.857 server (protobuf voice), 01-02 hit a
/// 1.3.4 server (legacy voice). This locks in that split so `decode_udp`'s
/// protobuf path stays validated against real >=1.5 traffic, and guards against a
/// regression that would silently reclassify protobuf Audio as legacy (or vice
/// versa) — the exact confusion that once led the handoff to call the whole
/// corpus 1.3.4.
#[test]
fn protobuf_server_scenarios_decode_real_protobuf_voice() {
    for scenario in PROTOBUF_SERVER_SCENARIOS {
        let (protobuf_audio, decrypted_legacy) = voice_plane_counts(scenario);
        assert!(
            protobuf_audio > 0,
            "{scenario}: expected real protobuf Audio packets (1.5.857 server), found none"
        );
        assert_eq!(
            decrypted_legacy, 0,
            "{scenario}: a 1.5 server must not yield legacy-format voice ({decrypted_legacy} found)"
        );
    }
}

/// Re-encoding a decoded message and decoding it again must reproduce it exactly.
/// This is the property the P2 MITM proxy relies on: it decodes each real message
/// and forwards a freshly-encoded copy, which the peer must decode to the same
/// value. Run over every control frame and UDP packet of all 7 captures, it
/// exercises `encode_control`/`encode_udp` against all real message types.
#[test]
fn every_decoded_message_reencodes_to_an_equal_message() {
    use mumble_server_runtime_protocol::{decode_control, decode_udp, encode_control, encode_udp};

    for scenario in SCENARIOS {
        let path = corpus_dir().join(scenario).join("session.voxcap");
        let records =
            read_records(&path).unwrap_or_else(|error| panic!("reading {scenario}: {error:#}"));
        let transcript = decode_session(&records)
            .unwrap_or_else(|error| panic!("decoding {scenario}: {error:#}"));

        for event in &transcript.events {
            match &event.decoded {
                Decoded::Control(message) => {
                    let (message_type, payload) = encode_control(message);
                    let redecoded = decode_control(message_type, &payload).unwrap_or_else(|error| {
                        panic!("{scenario}: re-decode of our own control encoding failed: {error:#}")
                    });
                    assert_eq!(
                        &redecoded,
                        message.as_ref(),
                        "{scenario}: control message changed across encode->decode"
                    );
                }
                Decoded::Udp(message) => {
                    let bytes = encode_udp(message);
                    let redecoded = decode_udp(&bytes).unwrap_or_else(|error| {
                        panic!("{scenario}: re-decode of our own UDP encoding failed: {error:#}")
                    });
                    assert_eq!(
                        &redecoded,
                        message.as_ref(),
                        "{scenario}: UDP message changed across encode->decode"
                    );
                }
                _ => {}
            }
        }
    }
}

#[test]
fn legacy_server_scenarios_stay_legacy() {
    for scenario in LEGACY_SERVER_SCENARIOS {
        let (protobuf_audio, decrypted_legacy) = voice_plane_counts(scenario);
        assert!(
            decrypted_legacy > 0,
            "{scenario}: expected decrypted legacy voice (1.3.4 server), found none"
        );
        assert_eq!(
            protobuf_audio, 0,
            "{scenario}: a 1.3.4 server must not yield protobuf Audio ({protobuf_audio} found)"
        );
    }
}
