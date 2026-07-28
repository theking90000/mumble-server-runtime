//! UDP voice-plane envelope decoding (protobuf format, Mumble 1.5+).
//!
//! A decrypted UDP packet is a one-byte type header followed by a protobuf
//! message:
//!
//! ```text
//! [ type: u8 ][ protobuf message ]
//! ```
//!
//! REF: references/vendored/protocol/MumbleProtocol.h : `MUMBLE_ALL_UDP_MESSAGES`
//!      (Audio = 0, Ping = 1).
//! REF: MumbleProtocol.cpp : `m_byteBuffer[0] = UDPMessageType::Audio/Ping`, protobuf
//!      encoded from offset 1; `UDPDecoder::decode` rejects `data.size() <= 1`.
//!
//! Only the protobuf format is implemented. The legacy UDP format is out of scope
//! per ADR-0001; a packet whose header is neither Audio nor Ping is rejected
//! rather than guessed (fail closed, R6). This decodes the envelope only: the
//! `opus_data` payload stays as raw bytes — no Opus decoding happens here.
//!
//! Input is assumed already decrypted; OCB2 lives in `mumble-server-runtime-crypto`.

use prost::Message;
use thiserror::Error;

use crate::messages::udp;

/// UDP message type register (protobuf format).
/// REF: MumbleProtocol.h : `enum class UDPMessageType : byte { Audio = 0, Ping = 1 }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum UdpMessageType {
    Audio = 0,
    Ping = 1,
}

impl From<UdpMessageType> for u8 {
    fn from(value: UdpMessageType) -> Self {
        value as u8
    }
}

/// A decoded UDP voice-plane message.
#[derive(Debug, Clone, PartialEq)]
pub enum UdpMessage {
    Audio(udp::Audio),
    Ping(udp::Ping),
}

/// Errors from decoding a UDP envelope.
#[derive(Debug, Error)]
pub enum UdpDecodeError {
    /// Empty, or only the header byte with no payload.
    /// REF: MumbleProtocol.cpp : `if (data.size() <= 1) return false;`.
    #[error("UDP packet too short: {len} byte(s), need a header plus payload")]
    TooShort { len: usize },

    /// The header byte is neither Audio (0) nor Ping (1). This includes every
    /// legacy-format packet, which is deliberately unsupported (ADR-0001).
    #[error(
        "unsupported UDP packet type {0}; only protobuf Audio (0) and Ping (1) \
         are implemented (legacy is out per ADR-0001)"
    )]
    UnsupportedType(u8),

    /// The payload was not a valid protobuf encoding of its declared message.
    #[error("protobuf decode failed for UDP {message_type:?} ({payload_len} bytes): {source}")]
    Protobuf {
        message_type: UdpMessageType,
        payload_len: usize,
        source: prost::DecodeError,
    },
}

/// Decode a decrypted UDP packet into a typed [`UdpMessage`].
pub fn decode_udp(packet: &[u8]) -> Result<UdpMessage, UdpDecodeError> {
    // A valid packet is a header byte plus a non-empty payload.
    let (&header, payload) = match packet.split_first() {
        Some(split) if !split.1.is_empty() => split,
        _ => return Err(UdpDecodeError::TooShort { len: packet.len() }),
    };

    let message_type = match header {
        0 => UdpMessageType::Audio,
        1 => UdpMessageType::Ping,
        other => return Err(UdpDecodeError::UnsupportedType(other)),
    };

    Ok(match message_type {
        UdpMessageType::Audio => UdpMessage::Audio(decode_pb(message_type, payload)?),
        UdpMessageType::Ping => UdpMessage::Ping(decode_pb(message_type, payload)?),
    })
}

/// Encode a UDP voice-plane message to a decrypted packet (`[type][protobuf]`) —
/// the inverse of [`decode_udp`], so `decode_udp(&encode_udp(m)) == Ok(m)`.
///
/// The result is plaintext; OCB2 encryption is the caller's job (`mumble-server-runtime-crypto`).
pub fn encode_udp(message: &UdpMessage) -> Vec<u8> {
    let (message_type, body) = match message {
        UdpMessage::Audio(audio) => (UdpMessageType::Audio, audio.encode_to_vec()),
        UdpMessage::Ping(ping) => (UdpMessageType::Ping, ping.encode_to_vec()),
    };
    // body is our own freshly-encoded output, not a wire-derived length.
    let mut packet = Vec::with_capacity(1 + body.len());
    packet.push(u8::from(message_type));
    packet.extend_from_slice(&body);
    packet
}

/// Prost-decode a payload, tagging failures with the message type and length.
fn decode_pb<M: Message + Default>(
    message_type: UdpMessageType,
    payload: &[u8],
) -> Result<M, UdpDecodeError> {
    M::decode(payload).map_err(|source| UdpDecodeError::Protobuf {
        message_type,
        payload_len: payload.len(),
        source,
    })
}

#[cfg(test)]
mod tests {
    // See framing.rs for why the test module allows expect_used under -D warnings.
    #![allow(clippy::expect_used)]

    use super::*;

    fn envelope(message_type: UdpMessageType, body: &[u8]) -> Vec<u8> {
        let mut packet = vec![u8::from(message_type)];
        packet.extend_from_slice(body);
        packet
    }

    #[test]
    fn decodes_audio_envelope_and_keeps_opus_raw() {
        let audio = udp::Audio {
            header: Some(udp::audio::Header::Target(0)),
            sender_session: 5,
            frame_number: 100,
            opus_data: vec![0xAA, 0xBB, 0xCC], // opaque Opus bytes, never decoded
            positional_data: vec![],
            volume_adjustment: 0.0,
            is_terminator: false,
        };
        let packet = envelope(UdpMessageType::Audio, &audio.encode_to_vec());
        match decode_udp(&packet).expect("decode audio") {
            UdpMessage::Audio(decoded) => {
                assert_eq!(decoded, audio);
                // The envelope decoder must not touch the Opus payload.
                assert_eq!(decoded.opus_data, vec![0xAA, 0xBB, 0xCC]);
            }
            other => panic!("expected Audio, got {other:?}"),
        }
    }

    #[test]
    fn decodes_ping_envelope() {
        let ping = udp::Ping {
            timestamp: 987_654,
            ..Default::default()
        };
        let packet = envelope(UdpMessageType::Ping, &ping.encode_to_vec());
        match decode_udp(&packet).expect("decode ping") {
            UdpMessage::Ping(decoded) => assert_eq!(decoded, ping),
            other => panic!("expected Ping, got {other:?}"),
        }
    }

    #[test]
    fn encode_then_decode_roundtrips_audio_and_ping() {
        let audio = UdpMessage::Audio(udp::Audio {
            header: Some(udp::audio::Header::Context(0)),
            sender_session: 7,
            frame_number: 690,
            opus_data: vec![0xD8, 0xED, 0x5C],
            positional_data: vec![],
            volume_adjustment: 0.0,
            is_terminator: false,
        });
        assert_eq!(
            decode_udp(&encode_udp(&audio)).expect("re-decode audio"),
            audio
        );

        let ping = UdpMessage::Ping(udp::Ping {
            timestamp: 68_900,
            ..Default::default()
        });
        assert_eq!(
            decode_udp(&encode_udp(&ping)).expect("re-decode ping"),
            ping
        );
    }

    #[test]
    fn encoded_header_byte_matches_message_type() {
        let ping = UdpMessage::Ping(udp::Ping {
            timestamp: 1,
            ..Default::default()
        });
        assert_eq!(encode_udp(&ping)[0], u8::from(UdpMessageType::Ping));
    }

    #[test]
    fn empty_and_header_only_packets_are_too_short() {
        match decode_udp(&[]) {
            Err(UdpDecodeError::TooShort { len: 0 }) => {}
            other => panic!("expected TooShort(0), got {other:?}"),
        }
        // A lone header byte with no payload is invalid.
        match decode_udp(&[0x00]) {
            Err(UdpDecodeError::TooShort { len: 1 }) => {}
            other => panic!("expected TooShort(1), got {other:?}"),
        }
    }

    #[test]
    fn legacy_or_unknown_header_fails_closed() {
        // A legacy Opus voice packet has its codec type in the high bits, e.g.
        // header 0x80. We do not support legacy: reject rather than misparse.
        match decode_udp(&[0x80, 0x01, 0x02]) {
            Err(UdpDecodeError::UnsupportedType(0x80)) => {}
            other => panic!("expected UnsupportedType(0x80), got {other:?}"),
        }
        match decode_udp(&[0x05, 0xFF]) {
            Err(UdpDecodeError::UnsupportedType(5)) => {}
            other => panic!("expected UnsupportedType(5), got {other:?}"),
        }
    }

    #[test]
    fn malformed_audio_payload_is_an_error() {
        // Header says Audio, but the payload is a truncated varint.
        match decode_udp(&[0x00, 0x08]) {
            Err(UdpDecodeError::Protobuf {
                message_type: UdpMessageType::Audio,
                ..
            }) => {}
            other => panic!("expected protobuf error, got {other:?}"),
        }
    }
}
