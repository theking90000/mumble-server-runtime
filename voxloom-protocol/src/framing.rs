//! TCP control-channel framing for the Mumble protocol.
//!
//! Every TCP message (after TLS termination) is length-prefixed with a fixed
//! six-byte header:
//!
//! ```text
//! [ type: u16 big-endian ][ length: u32 big-endian ][ payload: `length` bytes ]
//! ```
//!
//! REF: references/vendored/protocol/Connection.cpp : Connection::socketRead
//!      (reads 6 header bytes, then `iPacketLength` payload bytes; incremental).
//! REF: references/vendored/protocol/Connection.cpp : `if (iPacketLength > 0x7fffff)`
//!      (a larger declared length is a "huge packet" and the peer is dropped).
//! REF: references/vendored/protocol/MumbleProtocol.h : TCPMessageType (type codes 0..=26).
//!
//! This module is deliberately dumb about semantics: it extracts the raw type
//! code and the payload bytes. Mapping the code to a known message type, and
//! rejecting unknown codes, is [`TcpMessageType::try_from`]'s job, kept separate
//! so a partial or unknown frame is never confused with a malformed one.

use thiserror::Error;

/// Fixed size of a TCP message header: a 2-byte type plus a 4-byte length.
/// REF: Connection.cpp : `unsigned char a_ucBuffer[6]`.
pub const HEADER_LEN: usize = 6;

/// Largest payload length the framer accepts. A declared length above this is
/// treated as hostile; the caller must drop the connection (fail closed, R6).
/// REF: Connection.cpp : `if (iPacketLength > 0x7fffff) { ... "huge packet" ... }`.
pub const MAX_PAYLOAD_LEN: u32 = 0x7f_ffff;

/// Errors produced while framing the TCP control channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FramingError {
    /// The header declared a payload longer than [`MAX_PAYLOAD_LEN`]. Carries the
    /// declared length so the caller can log which connection misbehaved.
    #[error("declared payload length {declared} exceeds maximum {max}")]
    PayloadTooLarge { declared: u32, max: u32 },

    /// The type code does not correspond to any known Mumble TCP message.
    #[error("unknown TCP message type code {0}")]
    UnknownMessageType(u16),
}

/// One complete TCP frame borrowed from an input buffer.
///
/// [`Frame::message_type`] is the raw wire code; use [`TcpMessageType::try_from`]
/// to resolve it to a known message. [`Frame::total_len`] is the number of bytes
/// the caller should consume from the front of the buffer before parsing again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    pub message_type: u16,
    pub payload: &'a [u8],
}

impl Frame<'_> {
    /// Total bytes this frame occupies on the wire (header plus payload). Never
    /// overflows: the payload length was bounded by [`MAX_PAYLOAD_LEN`] at parse.
    pub fn total_len(&self) -> usize {
        HEADER_LEN + self.payload.len()
    }
}

/// Try to parse a single frame from the front of `buf`.
///
/// - `Ok(Some(frame))` — a complete frame is present; consume `frame.total_len()`
///   bytes and call again for the next one.
/// - `Ok(None)` — the buffer holds only a partial frame; append more bytes and
///   retry. This is how incremental parsing over arbitrarily split TCP chunks
///   works: the same partial buffer is re-offered once it has grown.
/// - `Err(_)` — a hard protocol violation; the connection must be dropped.
pub fn parse_frame(buf: &[u8]) -> Result<Option<Frame<'_>>, FramingError> {
    let header = match buf.get(..HEADER_LEN) {
        Some(header) => header,
        None => return Ok(None),
    };

    // `header` is exactly HEADER_LEN bytes, so these fixed indices are in bounds.
    let message_type = u16::from_be_bytes([header[0], header[1]]);
    let declared = u32::from_be_bytes([header[2], header[3], header[4], header[5]]);

    if declared > MAX_PAYLOAD_LEN {
        return Err(FramingError::PayloadTooLarge {
            declared,
            max: MAX_PAYLOAD_LEN,
        });
    }

    // `declared <= MAX_PAYLOAD_LEN` (well within usize), so both conversions and
    // the addition below cannot overflow, but we still check rather than assume.
    let payload_len = usize::try_from(declared).map_err(|_| FramingError::PayloadTooLarge {
        declared,
        max: MAX_PAYLOAD_LEN,
    })?;
    let end = HEADER_LEN
        .checked_add(payload_len)
        .ok_or(FramingError::PayloadTooLarge {
            declared,
            max: MAX_PAYLOAD_LEN,
        })?;

    match buf.get(HEADER_LEN..end) {
        Some(payload) => Ok(Some(Frame {
            message_type,
            payload,
        })),
        None => Ok(None),
    }
}

/// Encode a frame (header plus payload) onto `out`. The inverse of
/// [`parse_frame`]; `parse_frame(write_frame(t, p)) == (t, p)`.
///
/// Refuses payloads longer than [`MAX_PAYLOAD_LEN`] rather than emitting a frame
/// the wire would reject.
pub fn write_frame(
    message_type: u16,
    payload: &[u8],
    out: &mut Vec<u8>,
) -> Result<(), FramingError> {
    let declared = u32::try_from(payload.len()).map_err(|_| FramingError::PayloadTooLarge {
        declared: MAX_PAYLOAD_LEN.saturating_add(1),
        max: MAX_PAYLOAD_LEN,
    })?;
    if declared > MAX_PAYLOAD_LEN {
        return Err(FramingError::PayloadTooLarge {
            declared,
            max: MAX_PAYLOAD_LEN,
        });
    }
    out.extend_from_slice(&message_type.to_be_bytes());
    out.extend_from_slice(&declared.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(())
}

/// The TCP message type register.
///
/// REF: references/vendored/protocol/MumbleProtocol.h : `MUMBLE_ALL_TCP_MESSAGES`
///      X-macro (`Version = 0` .. `PluginDataTransmission = 26`). Variant names
///      are idiomatic Rust; the numeric codes are the wire truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum TcpMessageType {
    Version = 0,
    /// Special case: the payload is raw UDP audio bytes, not a protobuf message.
    /// REF: ServerHandler.cpp : `memcpy(uc + 6, data, len)` under `UDPTunnel`.
    UdpTunnel = 1,
    Authenticate = 2,
    Ping = 3,
    Reject = 4,
    ServerSync = 5,
    ChannelRemove = 6,
    ChannelState = 7,
    UserRemove = 8,
    UserState = 9,
    BanList = 10,
    TextMessage = 11,
    PermissionDenied = 12,
    Acl = 13,
    QueryUsers = 14,
    CryptSetup = 15,
    ContextActionModify = 16,
    ContextAction = 17,
    UserList = 18,
    VoiceTarget = 19,
    PermissionQuery = 20,
    CodecVersion = 21,
    UserStats = 22,
    RequestBlob = 23,
    ServerConfig = 24,
    SuggestConfig = 25,
    PluginDataTransmission = 26,
}

impl TryFrom<u16> for TcpMessageType {
    type Error = FramingError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => Self::Version,
            1 => Self::UdpTunnel,
            2 => Self::Authenticate,
            3 => Self::Ping,
            4 => Self::Reject,
            5 => Self::ServerSync,
            6 => Self::ChannelRemove,
            7 => Self::ChannelState,
            8 => Self::UserRemove,
            9 => Self::UserState,
            10 => Self::BanList,
            11 => Self::TextMessage,
            12 => Self::PermissionDenied,
            13 => Self::Acl,
            14 => Self::QueryUsers,
            15 => Self::CryptSetup,
            16 => Self::ContextActionModify,
            17 => Self::ContextAction,
            18 => Self::UserList,
            19 => Self::VoiceTarget,
            20 => Self::PermissionQuery,
            21 => Self::CodecVersion,
            22 => Self::UserStats,
            23 => Self::RequestBlob,
            24 => Self::ServerConfig,
            25 => Self::SuggestConfig,
            26 => Self::PluginDataTransmission,
            other => return Err(FramingError::UnknownMessageType(other)),
        })
    }
}

impl From<TcpMessageType> for u16 {
    fn from(value: TcpMessageType) -> Self {
        value as u16
    }
}

#[cfg(test)]
mod tests {
    // `expect_used` is workspace-warn (allowed but monitored) and CI runs with
    // `-D warnings`, which would promote it to an error. Tests legitimately use
    // `.expect(msg)` to assert Results; production code above stays strict.
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn empty_buffer_needs_more() {
        assert_eq!(parse_frame(&[]).expect("no error"), None);
    }

    #[test]
    fn partial_header_needs_more() {
        // Five bytes: one short of a header.
        assert_eq!(parse_frame(&[0, 1, 0, 0, 0]).expect("no error"), None);
    }

    #[test]
    fn partial_payload_needs_more() {
        // Header declares 4 payload bytes but only 2 are present.
        let buf = [0x00, 0x09, 0x00, 0x00, 0x00, 0x04, 0xAA, 0xBB];
        assert_eq!(parse_frame(&buf).expect("no error"), None);
    }

    #[test]
    fn parses_complete_frame() {
        // type = 9 (UserState), length = 3, payload = [1,2,3].
        let buf = [0x00, 0x09, 0x00, 0x00, 0x00, 0x03, 1, 2, 3];
        let frame = parse_frame(&buf).expect("no error").expect("a frame");
        assert_eq!(frame.message_type, 9);
        assert_eq!(frame.payload, &[1, 2, 3]);
        assert_eq!(frame.total_len(), 9);
    }

    #[test]
    fn parses_empty_payload_frame() {
        // A zero-length payload is legal (e.g. an empty Ping body).
        let buf = [0x00, 0x03, 0x00, 0x00, 0x00, 0x00];
        let frame = parse_frame(&buf).expect("no error").expect("a frame");
        assert_eq!(frame.message_type, 3);
        assert_eq!(frame.payload, &[] as &[u8]);
        assert_eq!(frame.total_len(), HEADER_LEN);
    }

    #[test]
    fn ignores_trailing_bytes_of_next_frame() {
        // One full frame followed by the start of another; only the first parses.
        let mut buf = Vec::new();
        write_frame(7, &[0xDE, 0xAD], &mut buf).expect("write");
        buf.extend_from_slice(&[0x00, 0x05]); // partial next header
        let frame = parse_frame(&buf).expect("no error").expect("a frame");
        assert_eq!(frame.message_type, 7);
        assert_eq!(frame.payload, &[0xDE, 0xAD]);
        assert_eq!(frame.total_len(), 8);
    }

    #[test]
    fn rejects_oversized_payload() {
        // Declared length MAX + 1.
        let declared = MAX_PAYLOAD_LEN + 1;
        let mut buf = vec![0x00, 0x0B];
        buf.extend_from_slice(&declared.to_be_bytes());
        assert_eq!(
            parse_frame(&buf),
            Err(FramingError::PayloadTooLarge {
                declared,
                max: MAX_PAYLOAD_LEN,
            })
        );
    }

    #[test]
    fn accepts_max_length_header() {
        // A header declaring exactly MAX is not itself an error; it just needs
        // that many payload bytes. With none present, the framer asks for more.
        let mut buf = vec![0x00, 0x0B];
        buf.extend_from_slice(&MAX_PAYLOAD_LEN.to_be_bytes());
        assert_eq!(parse_frame(&buf).expect("not too large"), None);
    }

    #[test]
    fn incremental_growth_yields_frame_once_complete() {
        let full = {
            let mut buf = Vec::new();
            write_frame(2, &[1, 2, 3, 4, 5], &mut buf).expect("write");
            buf
        };
        // Feeding one byte at a time returns None until the last byte arrives.
        for len in 0..full.len() {
            assert_eq!(
                parse_frame(&full[..len]).expect("no error"),
                None,
                "len {len}"
            );
        }
        let frame = parse_frame(&full).expect("no error").expect("a frame");
        assert_eq!(frame.message_type, 2);
        assert_eq!(frame.payload, &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn drains_multiple_frames_in_sequence() {
        let mut buf = Vec::new();
        write_frame(0, b"ver", &mut buf).expect("write");
        write_frame(3, b"", &mut buf).expect("write");
        write_frame(9, b"user", &mut buf).expect("write");

        let mut cursor = &buf[..];
        let mut seen = Vec::new();
        while let Some(frame) = parse_frame(cursor).expect("no error") {
            seen.push((frame.message_type, frame.payload.to_vec()));
            cursor = &cursor[frame.total_len()..];
        }
        assert_eq!(
            seen,
            vec![
                (0u16, b"ver".to_vec()),
                (3, b"".to_vec()),
                (9, b"user".to_vec()),
            ]
        );
        assert!(cursor.is_empty());
    }

    #[test]
    fn roundtrip_write_then_parse() {
        for message_type in 0u16..=26 {
            let payload: Vec<u8> = (0..message_type as usize).map(|i| i as u8).collect();
            let mut buf = Vec::new();
            write_frame(message_type, &payload, &mut buf).expect("write");
            let frame = parse_frame(&buf).expect("no error").expect("a frame");
            assert_eq!(frame.message_type, message_type);
            assert_eq!(frame.payload, &payload[..]);
        }
    }

    #[test]
    fn all_known_type_codes_resolve() {
        for code in 0u16..=26 {
            let resolved = TcpMessageType::try_from(code).expect("known code");
            assert_eq!(u16::from(resolved), code);
        }
    }

    #[test]
    fn unknown_type_code_is_rejected() {
        assert_eq!(
            TcpMessageType::try_from(27),
            Err(FramingError::UnknownMessageType(27))
        );
        assert_eq!(
            TcpMessageType::try_from(u16::MAX),
            Err(FramingError::UnknownMessageType(u16::MAX))
        );
    }

    #[test]
    fn udp_tunnel_is_type_one() {
        // The audio-tunnel special case must stay at code 1.
        // REF: MumbleProtocol.h : PROCESS_MUMBLE_TCP_MESSAGE(UDPTunnel, 1).
        assert_eq!(u16::from(TcpMessageType::UdpTunnel), 1);
        assert_eq!(
            TcpMessageType::try_from(1).expect("known"),
            TcpMessageType::UdpTunnel
        );
    }
}
