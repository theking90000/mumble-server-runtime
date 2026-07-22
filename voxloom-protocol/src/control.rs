//! Typed decoding of TCP control-channel messages.
//!
//! [`decode_control`] maps a framed message (type code plus payload) to a typed
//! [`ControlMessage`]. Every type prost-decodes its payload except one:
//!
//! REF: references/vendored/Mumble.proto : `message UDPTunnel` — "Not used".
//! REF: references/vendored/protocol/Connection.cpp / ServerHandler.cpp : a TCP
//!      frame of type `UDPTunnel` (1) carries a raw UDP audio packet at offset 6,
//!      NOT an encoded `UDPTunnel` protobuf message. [`ControlMessage::UdpTunnel`]
//!      therefore holds the raw bytes verbatim. `udp_tunnel_*` tests below fail if
//!      this special case is ever "fixed" into a protobuf decode.

use prost::Message;
use thiserror::Error;

use crate::framing::{Frame, FramingError, TcpMessageType, write_frame};
use crate::messages::tcp;

/// A decoded TCP control-channel message.
#[derive(Debug, Clone, PartialEq)]
pub enum ControlMessage {
    Version(tcp::Version),
    /// Raw UDP audio bytes tunneled over TCP — not the (unused) `UDPTunnel`
    /// protobuf message. See the module REF notes.
    UdpTunnel(Vec<u8>),
    Authenticate(tcp::Authenticate),
    Ping(tcp::Ping),
    Reject(tcp::Reject),
    ServerSync(tcp::ServerSync),
    ChannelRemove(tcp::ChannelRemove),
    ChannelState(tcp::ChannelState),
    UserRemove(tcp::UserRemove),
    UserState(tcp::UserState),
    BanList(tcp::BanList),
    TextMessage(tcp::TextMessage),
    PermissionDenied(tcp::PermissionDenied),
    Acl(tcp::Acl),
    QueryUsers(tcp::QueryUsers),
    CryptSetup(tcp::CryptSetup),
    ContextActionModify(tcp::ContextActionModify),
    ContextAction(tcp::ContextAction),
    UserList(tcp::UserList),
    VoiceTarget(tcp::VoiceTarget),
    PermissionQuery(tcp::PermissionQuery),
    CodecVersion(tcp::CodecVersion),
    UserStats(tcp::UserStats),
    RequestBlob(tcp::RequestBlob),
    ServerConfig(tcp::ServerConfig),
    SuggestConfig(tcp::SuggestConfig),
    PluginDataTransmission(tcp::PluginDataTransmission),
}

/// Errors from decoding a control-channel message.
#[derive(Debug, Error)]
pub enum DecodeError {
    /// The frame's type code is not a known Mumble TCP message.
    #[error(transparent)]
    Framing(#[from] FramingError),

    /// The payload was not a valid protobuf encoding of its declared message.
    #[error("protobuf decode failed for {message_type:?} ({payload_len} bytes): {source}")]
    Protobuf {
        message_type: TcpMessageType,
        payload_len: usize,
        source: prost::DecodeError,
    },
}

/// Decode a control-channel message from a raw type code and payload.
///
/// Fails closed: an unknown type code or a malformed protobuf payload is an
/// error, never a silently accepted message (R6).
pub fn decode_control(message_type: u16, payload: &[u8]) -> Result<ControlMessage, DecodeError> {
    let message_type = TcpMessageType::try_from(message_type)?;
    Ok(match message_type {
        // The load-bearing special case: raw audio, not a protobuf message.
        TcpMessageType::UdpTunnel => ControlMessage::UdpTunnel(payload.to_vec()),

        TcpMessageType::Version => ControlMessage::Version(decode_pb(message_type, payload)?),
        TcpMessageType::Authenticate => {
            ControlMessage::Authenticate(decode_pb(message_type, payload)?)
        }
        TcpMessageType::Ping => ControlMessage::Ping(decode_pb(message_type, payload)?),
        TcpMessageType::Reject => ControlMessage::Reject(decode_pb(message_type, payload)?),
        TcpMessageType::ServerSync => ControlMessage::ServerSync(decode_pb(message_type, payload)?),
        TcpMessageType::ChannelRemove => {
            ControlMessage::ChannelRemove(decode_pb(message_type, payload)?)
        }
        TcpMessageType::ChannelState => {
            ControlMessage::ChannelState(decode_pb(message_type, payload)?)
        }
        TcpMessageType::UserRemove => ControlMessage::UserRemove(decode_pb(message_type, payload)?),
        TcpMessageType::UserState => ControlMessage::UserState(decode_pb(message_type, payload)?),
        TcpMessageType::BanList => ControlMessage::BanList(decode_pb(message_type, payload)?),
        TcpMessageType::TextMessage => {
            ControlMessage::TextMessage(decode_pb(message_type, payload)?)
        }
        TcpMessageType::PermissionDenied => {
            ControlMessage::PermissionDenied(decode_pb(message_type, payload)?)
        }
        TcpMessageType::Acl => ControlMessage::Acl(decode_pb(message_type, payload)?),
        TcpMessageType::QueryUsers => ControlMessage::QueryUsers(decode_pb(message_type, payload)?),
        TcpMessageType::CryptSetup => ControlMessage::CryptSetup(decode_pb(message_type, payload)?),
        TcpMessageType::ContextActionModify => {
            ControlMessage::ContextActionModify(decode_pb(message_type, payload)?)
        }
        TcpMessageType::ContextAction => {
            ControlMessage::ContextAction(decode_pb(message_type, payload)?)
        }
        TcpMessageType::UserList => ControlMessage::UserList(decode_pb(message_type, payload)?),
        TcpMessageType::VoiceTarget => {
            ControlMessage::VoiceTarget(decode_pb(message_type, payload)?)
        }
        TcpMessageType::PermissionQuery => {
            ControlMessage::PermissionQuery(decode_pb(message_type, payload)?)
        }
        TcpMessageType::CodecVersion => {
            ControlMessage::CodecVersion(decode_pb(message_type, payload)?)
        }
        TcpMessageType::UserStats => ControlMessage::UserStats(decode_pb(message_type, payload)?),
        TcpMessageType::RequestBlob => {
            ControlMessage::RequestBlob(decode_pb(message_type, payload)?)
        }
        TcpMessageType::ServerConfig => {
            ControlMessage::ServerConfig(decode_pb(message_type, payload)?)
        }
        TcpMessageType::SuggestConfig => {
            ControlMessage::SuggestConfig(decode_pb(message_type, payload)?)
        }
        TcpMessageType::PluginDataTransmission => {
            ControlMessage::PluginDataTransmission(decode_pb(message_type, payload)?)
        }
    })
}

/// Decode a control-channel message directly from a parsed [`Frame`].
pub fn decode_frame(frame: &Frame<'_>) -> Result<ControlMessage, DecodeError> {
    decode_control(frame.message_type, frame.payload)
}

/// Encode a control message to its wire type code and payload — the inverse of
/// [`decode_control`], so `decode_control(encode_control(m)) == m`.
///
/// The `UdpTunnel` special case is preserved in reverse: its bytes are the raw
/// tunneled audio, emitted verbatim, never wrapped in the (unused) `UDPTunnel`
/// protobuf message.
pub fn encode_control(message: &ControlMessage) -> (u16, Vec<u8>) {
    match message {
        // The load-bearing special case: raw audio, emitted as-is.
        ControlMessage::UdpTunnel(bytes) => (u16::from(TcpMessageType::UdpTunnel), bytes.clone()),

        ControlMessage::Version(m) => encode_pb(TcpMessageType::Version, m),
        ControlMessage::Authenticate(m) => encode_pb(TcpMessageType::Authenticate, m),
        ControlMessage::Ping(m) => encode_pb(TcpMessageType::Ping, m),
        ControlMessage::Reject(m) => encode_pb(TcpMessageType::Reject, m),
        ControlMessage::ServerSync(m) => encode_pb(TcpMessageType::ServerSync, m),
        ControlMessage::ChannelRemove(m) => encode_pb(TcpMessageType::ChannelRemove, m),
        ControlMessage::ChannelState(m) => encode_pb(TcpMessageType::ChannelState, m),
        ControlMessage::UserRemove(m) => encode_pb(TcpMessageType::UserRemove, m),
        ControlMessage::UserState(m) => encode_pb(TcpMessageType::UserState, m),
        ControlMessage::BanList(m) => encode_pb(TcpMessageType::BanList, m),
        ControlMessage::TextMessage(m) => encode_pb(TcpMessageType::TextMessage, m),
        ControlMessage::PermissionDenied(m) => encode_pb(TcpMessageType::PermissionDenied, m),
        ControlMessage::Acl(m) => encode_pb(TcpMessageType::Acl, m),
        ControlMessage::QueryUsers(m) => encode_pb(TcpMessageType::QueryUsers, m),
        ControlMessage::CryptSetup(m) => encode_pb(TcpMessageType::CryptSetup, m),
        ControlMessage::ContextActionModify(m) => encode_pb(TcpMessageType::ContextActionModify, m),
        ControlMessage::ContextAction(m) => encode_pb(TcpMessageType::ContextAction, m),
        ControlMessage::UserList(m) => encode_pb(TcpMessageType::UserList, m),
        ControlMessage::VoiceTarget(m) => encode_pb(TcpMessageType::VoiceTarget, m),
        ControlMessage::PermissionQuery(m) => encode_pb(TcpMessageType::PermissionQuery, m),
        ControlMessage::CodecVersion(m) => encode_pb(TcpMessageType::CodecVersion, m),
        ControlMessage::UserStats(m) => encode_pb(TcpMessageType::UserStats, m),
        ControlMessage::RequestBlob(m) => encode_pb(TcpMessageType::RequestBlob, m),
        ControlMessage::ServerConfig(m) => encode_pb(TcpMessageType::ServerConfig, m),
        ControlMessage::SuggestConfig(m) => encode_pb(TcpMessageType::SuggestConfig, m),
        ControlMessage::PluginDataTransmission(m) => {
            encode_pb(TcpMessageType::PluginDataTransmission, m)
        }
    }
}

/// Encode a control message as a complete TCP frame (6-byte header plus payload)
/// appended to `out`. Composes [`encode_control`] with [`write_frame`], so
/// `decode_frame(parse_frame(encode_frame(m))) == m`.
pub fn encode_frame(message: &ControlMessage, out: &mut Vec<u8>) -> Result<(), FramingError> {
    let (message_type, payload) = encode_control(message);
    write_frame(message_type, &payload, out)
}

/// Prost-encode a message and tag it with its wire type code.
fn encode_pb<M: Message>(message_type: TcpMessageType, message: &M) -> (u16, Vec<u8>) {
    (u16::from(message_type), message.encode_to_vec())
}

/// Prost-decode a payload into a message, attaching the message type and length
/// on failure so a decode error names what and where it went wrong.
fn decode_pb<M: Message + Default>(
    message_type: TcpMessageType,
    payload: &[u8],
) -> Result<M, DecodeError> {
    M::decode(payload).map_err(|source| DecodeError::Protobuf {
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

    #[test]
    fn decodes_a_normal_protobuf_message() {
        let version = tcp::Version {
            release: Some("voxloom".to_string()),
            ..Default::default()
        };
        let decoded = decode_control(0, &version.encode_to_vec()).expect("decode Version");
        assert_eq!(decoded, ControlMessage::Version(version));
    }

    #[test]
    fn udp_tunnel_payload_is_returned_raw() {
        // A raw audio packet, deliberately not a valid `UDPTunnel` protobuf.
        let raw_audio = vec![0x80u8, 0x00, 0xDE, 0xAD, 0xBE, 0xEF, 0xFF];
        let decoded = decode_control(1, &raw_audio).expect("decode UDPTunnel");
        assert_eq!(decoded, ControlMessage::UdpTunnel(raw_audio));
    }

    #[test]
    fn udp_tunnel_is_raw_even_when_bytes_are_valid_protobuf() {
        // The strongest form of the guard: bytes that ARE a valid `UDPTunnel`
        // protobuf must still come back verbatim as raw audio, never unwrapped
        // into the protobuf's `packet` field. If the dispatch is ever changed to
        // prost-decode type 1, this assertion breaks.
        let looks_like_protobuf = tcp::UdpTunnel {
            packet: vec![0xDE, 0xAD, 0xBE, 0xEF],
        }
        .encode_to_vec();
        let decoded = decode_control(1, &looks_like_protobuf).expect("decode UDPTunnel");
        assert_eq!(decoded, ControlMessage::UdpTunnel(looks_like_protobuf));
    }

    #[test]
    fn unknown_type_code_fails_closed() {
        match decode_control(99, &[]) {
            Err(DecodeError::Framing(FramingError::UnknownMessageType(99))) => {}
            other => panic!("expected unknown-type error, got {other:?}"),
        }
    }

    #[test]
    fn encode_then_decode_roundtrips_a_protobuf_message() {
        let message = ControlMessage::ServerSync(tcp::ServerSync {
            session: Some(42),
            max_bandwidth: Some(72_000),
            welcome_text: Some("hi".to_string()),
            permissions: Some(0),
        });
        let (message_type, payload) = encode_control(&message);
        assert_eq!(
            decode_control(message_type, &payload).expect("re-decode"),
            message
        );
    }

    #[test]
    fn encode_preserves_udp_tunnel_raw_bytes() {
        // The raw-audio special case must survive a round trip byte-for-byte and
        // stay on type code 1, never become a protobuf-encoded UDPTunnel.
        let raw_audio = vec![0x00u8, 0x18, 0x02, 0x20, 0xB0, 0x05, 0xDE, 0xAD];
        let message = ControlMessage::UdpTunnel(raw_audio.clone());
        let (message_type, payload) = encode_control(&message);
        assert_eq!(message_type, u16::from(TcpMessageType::UdpTunnel));
        assert_eq!(payload, raw_audio);
        assert_eq!(
            decode_control(message_type, &payload).expect("re-decode"),
            message
        );
    }

    #[test]
    fn encode_frame_then_parse_roundtrips() {
        let message = ControlMessage::Version(tcp::Version {
            release: Some("voxloom".to_string()),
            ..Default::default()
        });
        let mut framed = Vec::new();
        encode_frame(&message, &mut framed).expect("encode frame");
        let frame = crate::framing::parse_frame(&framed)
            .expect("parse")
            .expect("a complete frame");
        assert_eq!(decode_frame(&frame).expect("decode frame"), message);
    }

    #[test]
    fn malformed_protobuf_is_an_error_not_a_default() {
        // A truncated varint for a Version field: prost must reject it rather
        // than yielding a default message.
        let malformed = [0x08u8]; // field 1, varint, but no value byte follows
        match decode_control(0, &malformed) {
            Err(DecodeError::Protobuf {
                message_type: TcpMessageType::Version,
                ..
            }) => {}
            other => panic!("expected protobuf decode error, got {other:?}"),
        }
    }
}
