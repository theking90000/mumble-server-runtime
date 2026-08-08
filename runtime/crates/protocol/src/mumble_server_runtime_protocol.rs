//! Codec pur du protocole Mumble (Phase 1) : framing TCP, messages protobuf,
//! enveloppes UDP. Aucune IO, aucun runtime (gates R4).
//!
//! La vérité protocolaire vient exclusivement des sources vendorées (R1) :
//! `runtime/references/vendored/` au commit `runtime/references/mumble.pin`. Chaque module porte
//! des commentaires `// REF:` traçant chaque fait vers ces sources.
#![forbid(unsafe_code)]

pub mod control;
pub mod framing;
pub mod messages;
pub mod udp;

pub use control::{
    ControlMessage, DecodeError, decode_control, decode_frame, encode_control, encode_frame,
};
pub use framing::{
    Frame, FramingError, HEADER_LEN, MAX_PAYLOAD_LEN, TcpMessageType, parse_frame, write_frame,
};
pub use udp::{UdpDecodeError, UdpMessage, UdpMessageType, decode_udp, encode_udp};
