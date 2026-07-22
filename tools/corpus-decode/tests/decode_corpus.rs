//! End-to-end decode of the real captured corpus. This is the Phase 1
//! done-criterion as an executable check: every scenario must decode with no
//! unexplained byte — every TCP stream fully framed, every UDP packet either
//! recognised as an unencrypted ping, OCB2-decrypted, or (never, for these
//! files) explicitly accounted as an OCB2 rejection.

use std::path::PathBuf;

use voxloom_corpus_decode::{decode_session, read_records};

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
