//! UDP voice-plane re-encryption: the heart of the Phase 2 oracle.
//!
//! Each encrypted datagram is decrypted under the cipher domain of the side it
//! came from, structurally validated by round-tripping through the Phase 1 codec
//! (`decode_udp` then `encode_udp`), then re-encrypted under the *other* side's
//! domain and forwarded. Because the proxy holds an independent OCB2 domain per
//! side (see [`crate::session`]), this is a true re-key, not a relay: what leaves
//! the proxy is ciphertext neither peer could have produced for the other.
//!
//! Unencrypted UDP connectivity pings are not part of the voice crypto and are
//! forwarded verbatim. Mumble decodes such pings on the raw bytes *before* it
//! ever attempts decryption, and the encrypted voice plane never collides with
//! their fixed shapes, so recognising them first is safe.
//! REF: runtime/references/mumble/src/murmur/Server.cpp : `Server::run` (`decodePing`
//!      then `checkDecrypt`).
//! REF: runtime/references/mumble/src/MumbleProtocol.cpp : `UDPDecoder::decodePing_legacy`
//!      (12-byte request with four leading zero bytes; 24-byte response) and
//!      `UDPDecoder::decode` (protobuf ping when `header == UDPMessageType::Ping`).
//!
//! Every datagram ends in exactly one explicit [`UdpOutcome`] — re-encrypted,
//! passed through, or dropped with a reason — never a silent discard (L4).

use mumble_server_runtime_crypto::CryptState;
use mumble_server_runtime_protocol::{UdpMessage, decode_udp, encode_udp};

use crate::relay::Origin;
use crate::session::CryptChannels;

/// What the UDP relay does with one datagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UdpOutcome {
    /// Voice (or an encrypted ping) re-encrypted toward the far side; forward
    /// these bytes.
    Reencrypted(Vec<u8>),
    /// An unencrypted connectivity ping; forward the original datagram verbatim.
    PassThrough,
    /// The datagram was not forwarded, for the stated reason. Fail closed.
    Dropped(DropReason),
}

/// Why a datagram was dropped rather than forwarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// OCB2 rejected it: a replay, an out-of-window late packet, or a tag
    /// mismatch. The real Murmur server drops exactly these the same way.
    /// REF: runtime/references/mumble/src/murmur/Server.cpp : `Server::checkDecrypt`.
    Ocb2Rejected,
    /// It decrypted, but the plaintext is not a protobuf UDP envelope: legacy
    /// voice (out of scope per ADR-0001) or corruption. The proxy cannot re-encode
    /// it faithfully, so it refuses to forward it.
    Undecodable,
    /// Re-encryption itself failed (the XEX* mitigation's authenticity edge). Not
    /// reachable for real voice, but handled explicitly rather than unwrapped.
    ReencryptFailed,
}

/// Re-encrypt a datagram travelling client -> server: decrypt with the client-
/// facing domain, re-encrypt with the server-facing domain.
pub fn reencrypt_from_client(channels: &mut CryptChannels, datagram: &[u8]) -> UdpOutcome {
    reencrypt(
        Origin::Client,
        &mut channels.to_client,
        &mut channels.to_server,
        datagram,
    )
}

/// Re-encrypt a datagram travelling server -> client: decrypt with the server-
/// facing domain, re-encrypt with the client-facing domain.
pub fn reencrypt_from_server(channels: &mut CryptChannels, datagram: &[u8]) -> UdpOutcome {
    reencrypt(
        Origin::Server,
        &mut channels.to_server,
        &mut channels.to_client,
        datagram,
    )
}

fn reencrypt(
    origin: Origin,
    decrypt_with: &mut CryptState,
    encrypt_with: &mut CryptState,
    datagram: &[u8],
) -> UdpOutcome {
    if is_connectivity_ping(origin, datagram) {
        return UdpOutcome::PassThrough;
    }
    let plaintext = match decrypt_with.decrypt(datagram) {
        Some(plaintext) => plaintext,
        None => return UdpOutcome::Dropped(DropReason::Ocb2Rejected),
    };
    // Validate structure by decoding, then re-encode canonically. For a real
    // protobuf message this is a no-op on the bytes; for anything else it fails
    // closed instead of forwarding a packet the proxy could not parse.
    let message = match decode_udp(&plaintext) {
        Ok(message) => message,
        Err(_) => return UdpOutcome::Dropped(DropReason::Undecodable),
    };
    let reencoded = encode_udp(&message);
    match encrypt_with.encrypt(&reencoded) {
        Some(ciphertext) => UdpOutcome::Reencrypted(ciphertext),
        None => UdpOutcome::Dropped(DropReason::ReencryptFailed),
    }
}

/// Recognise an unencrypted UDP connectivity ping by its fixed shapes. An
/// encrypted voice packet never matches these (its first byte is an OCB2 IV, and
/// its bytes do not cleanly decode as a `Ping`), so a match means "not voice".
///
/// The async relay ([`crate::udp_relay`]) also needs this to forward pings that
/// arrive before a session's cipher domains exist, so it is crate-visible.
pub(crate) fn is_connectivity_ping(origin: Origin, datagram: &[u8]) -> bool {
    // Legacy connectivity ping: direction-specific fixed shapes.
    match origin {
        // 12-byte request: four zero bytes then a 64-bit timestamp.
        Origin::Client => {
            if datagram.len() == 12 && datagram.starts_with(&[0, 0, 0, 0]) {
                return true;
            }
        }
        // 24-byte response: six big-endian u32 fields.
        Origin::Server => {
            if datagram.len() == 24 {
                return true;
            }
        }
    }
    // Unencrypted protobuf ping: header byte 0x01 and a clean MumbleUDP.Ping.
    datagram.first() == Some(&1) && matches!(decode_udp(datagram), Ok(UdpMessage::Ping(_)))
}

#[cfg(test)]
mod tests {
    // See framing.rs for why the test module allows expect_used under -D warnings.
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::session::{ProxySecrets, Session};
    use mumble_server_runtime_protocol::messages::tcp;
    use mumble_server_runtime_protocol::{ControlMessage, encode_udp};

    const SERVER_KEY: [u8; 16] = [0x51; 16];
    const SERVER_NONCE: [u8; 16] = [0x52; 16];
    const CLIENT_NONCE: [u8; 16] = [0x53; 16];
    const PROXY_KEY: [u8; 16] = [0x61; 16];
    const PROXY_ENCRYPT: [u8; 16] = [0x62; 16];
    const PROXY_DECRYPT: [u8; 16] = [0x63; 16];

    fn established_session() -> Session {
        let mut session = Session::new(ProxySecrets::new(PROXY_KEY, PROXY_ENCRYPT, PROXY_DECRYPT));
        session
            .process_from_server(ControlMessage::CryptSetup(tcp::CryptSetup {
                key: Some(SERVER_KEY.to_vec()),
                server_nonce: Some(SERVER_NONCE.to_vec()),
                client_nonce: Some(CLIENT_NONCE.to_vec()),
            }))
            .expect("establish");
        session
    }

    fn sample_audio() -> Vec<u8> {
        encode_udp(&UdpMessage::Audio(
            mumble_server_runtime_protocol::messages::udp::Audio {
                header: Some(
                    mumble_server_runtime_protocol::messages::udp::audio::Header::Target(0),
                ),
                sender_session: 3,
                frame_number: 42,
                opus_data: vec![0x11, 0x22, 0x33],
                positional_data: vec![],
                volume_adjustment: 0.0,
                is_terminator: false,
            },
        ))
    }

    #[test]
    fn server_to_client_voice_reencrypts_losslessly() {
        let mut session = established_session();
        let channels = session.channels_mut().expect("channels");

        // The real server encrypts with SERVER_NONCE / decrypts with CLIENT_NONCE.
        let mut real_server = CryptState::new(&SERVER_KEY, &SERVER_NONCE, &CLIENT_NONCE);
        // The re-keyed client decrypts proxy->client with PROXY_ENCRYPT.
        let mut real_client = CryptState::new(&PROXY_KEY, &PROXY_DECRYPT, &PROXY_ENCRYPT);

        let plaintext = sample_audio();
        let on_wire = real_server.encrypt(&plaintext).expect("server encrypt");

        match reencrypt_from_server(channels, &on_wire) {
            UdpOutcome::Reencrypted(out) => {
                let delivered = real_client.decrypt(&out).expect("client decrypt");
                assert_eq!(
                    decode_udp(&delivered).expect("decode delivered"),
                    decode_udp(&plaintext).expect("decode original"),
                );
            }
            other => panic!("expected re-encrypted voice, got {other:?}"),
        }
    }

    #[test]
    fn client_to_server_voice_reencrypts_losslessly() {
        let mut session = established_session();
        let channels = session.channels_mut().expect("channels");

        // The re-keyed client encrypts client->proxy with PROXY_DECRYPT.
        let mut real_client = CryptState::new(&PROXY_KEY, &PROXY_DECRYPT, &PROXY_ENCRYPT);
        // The real server decrypts client->server with CLIENT_NONCE.
        let mut real_server = CryptState::new(&SERVER_KEY, &SERVER_NONCE, &CLIENT_NONCE);

        let plaintext = sample_audio();
        let on_wire = real_client.encrypt(&plaintext).expect("client encrypt");

        match reencrypt_from_client(channels, &on_wire) {
            UdpOutcome::Reencrypted(out) => {
                let delivered = real_server.decrypt(&out).expect("server decrypt");
                assert_eq!(
                    decode_udp(&delivered).expect("decode delivered"),
                    decode_udp(&plaintext).expect("decode original"),
                );
            }
            other => panic!("expected re-encrypted voice, got {other:?}"),
        }
    }

    #[test]
    fn ocb2_reject_is_a_drop_not_a_forward() {
        let mut session = established_session();
        let channels = session.channels_mut().expect("channels");
        // 4+ bytes that are not a ping shape and will not authenticate.
        let garbage = [0x07u8, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
        assert_eq!(
            reencrypt_from_server(channels, &garbage),
            UdpOutcome::Dropped(DropReason::Ocb2Rejected),
        );
    }

    #[test]
    fn legacy_connectivity_ping_passes_through() {
        let mut session = established_session();
        let channels = session.channels_mut().expect("channels");
        // 12-byte client request: four zero bytes then a timestamp.
        let mut request = vec![0u8, 0, 0, 0];
        request.extend_from_slice(&1_234_567u64.to_be_bytes());
        assert_eq!(
            reencrypt_from_client(channels, &request),
            UdpOutcome::PassThrough,
        );
        // 24-byte server response.
        let response = vec![0u8; 24];
        assert_eq!(
            reencrypt_from_server(channels, &response),
            UdpOutcome::PassThrough,
        );
    }
}
