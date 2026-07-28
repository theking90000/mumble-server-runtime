//! Phase 2 tranche T3 done-command: replay the real captured corpus through the
//! UDP re-encryptor and prove the decrypt -> re-encode -> re-encrypt -> decrypt
//! loop is lossless, with zero rejects among the packets the real endpoint
//! accepted.
//!
//! The corpus is a single real session where client and server share one OCB2
//! key. To exercise the proxy's two-domain re-encryption over these real
//! packets, each direction is replayed with the proxy's *decrypting* domain set
//! to the real session key (so real ciphertext decrypts) and its *encrypting*
//! domain set to fresh proxy secrets (a genuine re-key). A reference endpoint,
//! built identically to the decrypting domain, provides ground-truth plaintext,
//! and a mirror of the fresh domain decrypts what the proxy emits. The two must
//! decode to the same message for every accepted packet.
//!
//! This reuses the Phase 1 corpus reader (`voxloom-corpus-decode`) and reads
//! fixtures without modifying them (R2/L2: verifier data is read, never touched).

#![allow(clippy::expect_used)]

use std::path::PathBuf;

use voxloom_corpus_decode::{Dir, Record, Transport, read_records};
use mumble_server_runtime_crypto::CryptState;
use voxloom_mitm_proxy::{
    CryptChannels, DropReason, UdpOutcome, reencrypt_from_client, reencrypt_from_server,
};
use mumble_server_runtime_protocol::{ControlMessage, decode_frame, decode_udp, parse_frame};

/// Scenarios captured against the local Mumble 1.5.857 server (protobuf voice).
/// 03 additionally carries a mid-session full re-key, exercising the rebuild path.
const PROTOBUF_SCENARIOS: &[&str] = &[
    "03-channel-create-remove",
    "04-two-clients-talking",
    "05-whisper",
    "06-permission-denied",
    "07-disconnect",
];

// Fixed fresh proxy secrets for the re-keyed (encrypting) domain. Distinct bytes
// so a domain mix-up cannot be masked by symmetry.
const PROXY_KEY: [u8; 16] = [0x61; 16];
const PROXY_ENCRYPT: [u8; 16] = [0x62; 16];
const PROXY_DECRYPT: [u8; 16] = [0x63; 16];

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/corpus")
}

#[derive(Clone, Copy)]
enum Tested {
    ClientToServer,
    ServerToClient,
}

impl Tested {
    fn matches(self, dir: Dir) -> bool {
        matches!(
            (self, dir),
            (Tested::ClientToServer, Dir::ClientToServer)
                | (Tested::ServerToClient, Dir::ServerToClient)
        )
    }
}

/// Reassembles one TCP direction and drains complete control frames.
struct Reassembler {
    buffer: Vec<u8>,
}

impl Reassembler {
    fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    fn push(&mut self, chunk: &[u8]) -> Vec<ControlMessage> {
        self.buffer.extend_from_slice(chunk);
        let mut messages = Vec::new();
        while let Some(frame) = parse_frame(&self.buffer).expect("frame the control stream") {
            let total = frame.total_len();
            messages.push(decode_frame(&frame).expect("decode control"));
            self.buffer.drain(..total);
        }
        messages
    }
}

/// A full CryptSetup carries the shared key and both nonces.
fn full_cryptsetup(message: &ControlMessage) -> Option<([u8; 16], [u8; 16], [u8; 16])> {
    let ControlMessage::CryptSetup(setup) = message else {
        return None;
    };
    let key = setup.key.as_deref()?.try_into().ok()?;
    let client_nonce = setup.client_nonce.as_deref()?.try_into().ok()?;
    let server_nonce = setup.server_nonce.as_deref()?.try_into().ok()?;
    Some((key, client_nonce, server_nonce))
}

/// The crypto for one replayed direction: the proxy's two domains, plus a
/// reference decryptor (ground truth) and a mirror of the re-keyed domain.
struct Crypto {
    channels: CryptChannels,
    reference: CryptState,
    mirror: CryptState,
}

impl Crypto {
    /// Build both domains at the first full CryptSetup.
    fn new(tested: Tested, key: [u8; 16], client_nonce: [u8; 16], server_nonce: [u8; 16]) -> Self {
        // The mirror decrypts whatever the fresh (encrypting) domain emits, whose
        // encrypt IV is PROXY_ENCRYPT in both directions.
        let mirror = CryptState::new(&PROXY_KEY, &PROXY_DECRYPT, &PROXY_ENCRYPT);
        let (channels, reference) = match tested {
            // Server -> client: to_server decrypts real s2c (decrypt IV =
            // server_nonce); to_client is the fresh re-keyed domain.
            Tested::ServerToClient => {
                let to_server = CryptState::new(&key, &client_nonce, &server_nonce);
                let to_client = CryptState::new(&PROXY_KEY, &PROXY_ENCRYPT, &PROXY_DECRYPT);
                let reference = CryptState::new(&key, &client_nonce, &server_nonce);
                (
                    CryptChannels {
                        to_server,
                        to_client,
                    },
                    reference,
                )
            }
            // Client -> server: to_client decrypts real c2s (decrypt IV =
            // client_nonce); to_server is the fresh re-keyed domain.
            Tested::ClientToServer => {
                let to_client = CryptState::new(&key, &server_nonce, &client_nonce);
                let to_server = CryptState::new(&PROXY_KEY, &PROXY_ENCRYPT, &PROXY_DECRYPT);
                let reference = CryptState::new(&key, &server_nonce, &client_nonce);
                (
                    CryptChannels {
                        to_server,
                        to_client,
                    },
                    reference,
                )
            }
        };
        Self {
            channels,
            reference,
            mirror,
        }
    }

    /// Adopt a mid-session re-key: rebuild only the corpus-key decrypting domain
    /// and the reference; the fresh re-keyed domain and its mirror carry over.
    fn rekey(
        &mut self,
        tested: Tested,
        key: [u8; 16],
        client_nonce: [u8; 16],
        server_nonce: [u8; 16],
    ) {
        match tested {
            Tested::ServerToClient => {
                self.channels.to_server = CryptState::new(&key, &client_nonce, &server_nonce);
                self.reference = CryptState::new(&key, &client_nonce, &server_nonce);
            }
            Tested::ClientToServer => {
                self.channels.to_client = CryptState::new(&key, &server_nonce, &client_nonce);
                self.reference = CryptState::new(&key, &server_nonce, &client_nonce);
            }
        }
    }
}

/// Replay one direction of one scenario. Returns (accepted, rejected).
fn replay(records: &[Record], tested: Tested) -> (usize, usize) {
    let mut c2s = Reassembler::new();
    let mut s2c = Reassembler::new();
    let mut crypto: Option<Crypto> = None;
    let mut accepted = 0;
    let mut rejected = 0;

    for record in records {
        match record.transport {
            Transport::Tcp => {
                let stream = match record.dir {
                    Dir::ClientToServer => &mut c2s,
                    Dir::ServerToClient => &mut s2c,
                };
                for message in stream.push(&record.data) {
                    if let Some((key, client_nonce, server_nonce)) = full_cryptsetup(&message) {
                        match crypto.as_mut() {
                            None => {
                                crypto = Some(Crypto::new(tested, key, client_nonce, server_nonce));
                            }
                            Some(existing) => {
                                existing.rekey(tested, key, client_nonce, server_nonce);
                            }
                        }
                    }
                }
            }
            Transport::Udp => {
                if !tested.matches(record.dir) {
                    continue;
                }
                // No crypto yet: only pre-handshake unencrypted pings can appear
                // here, and they are not part of the voice plane. Skip them.
                let Some(crypto) = crypto.as_mut() else {
                    continue;
                };
                let outcome = match tested {
                    Tested::ClientToServer => {
                        reencrypt_from_client(&mut crypto.channels, &record.data)
                    }
                    Tested::ServerToClient => {
                        reencrypt_from_server(&mut crypto.channels, &record.data)
                    }
                };
                match outcome {
                    // Unencrypted connectivity ping: neither the proxy's decrypt
                    // domain nor the reference advanced, so they stay in lockstep.
                    UdpOutcome::PassThrough => {}
                    UdpOutcome::Dropped(DropReason::Ocb2Rejected) => {
                        assert!(
                            crypto.reference.decrypt(&record.data).is_none(),
                            "proxy rejected a packet the real endpoint accepted",
                        );
                        rejected += 1;
                    }
                    UdpOutcome::Dropped(other) => {
                        panic!("unexpected drop of protobuf voice: {other:?}")
                    }
                    UdpOutcome::Reencrypted(reencrypted) => {
                        let ground_truth = crypto
                            .reference
                            .decrypt(&record.data)
                            .expect("reference decrypts an accepted packet");
                        let delivered = crypto
                            .mirror
                            .decrypt(&reencrypted)
                            .expect("mirror decrypts the re-encrypted packet");
                        assert_eq!(
                            decode_udp(&delivered).expect("decode delivered"),
                            decode_udp(&ground_truth).expect("decode ground truth"),
                            "re-encryption changed the voice message",
                        );
                        accepted += 1;
                    }
                }
            }
        }
    }

    (accepted, rejected)
}

#[test]
fn corpus_reencrypts_losslessly_with_zero_rejects() {
    let mut total_c2s = 0;
    let mut total_s2c = 0;

    for scenario in PROTOBUF_SCENARIOS {
        let path = corpus_dir().join(scenario).join("session.voxcap");
        let records =
            read_records(&path).unwrap_or_else(|error| panic!("reading {scenario}: {error:#}"));

        let (accepted_c2s, rejected_c2s) = replay(&records, Tested::ClientToServer);
        let (accepted_s2c, rejected_s2c) = replay(&records, Tested::ServerToClient);

        assert_eq!(
            rejected_c2s, 0,
            "{scenario}: {rejected_c2s} client->server voice packets rejected by the proxy",
        );
        assert_eq!(
            rejected_s2c, 0,
            "{scenario}: {rejected_s2c} server->client voice packets rejected by the proxy",
        );

        total_c2s += accepted_c2s;
        total_s2c += accepted_s2c;
    }

    assert!(
        total_c2s > 0,
        "no client->server voice was re-encrypted across the corpus",
    );
    assert!(
        total_s2c > 0,
        "no server->client voice was re-encrypted across the corpus",
    );
}
