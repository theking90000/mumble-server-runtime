//! Codec pur du protocole Mumble (Phase 1) : framing TCP, messages protobuf,
//! enveloppes UDP. Aucune IO, aucun runtime (gates R4).
//!
//! La vérité protocolaire vient exclusivement des sources vendorées (R1) :
//! `references/vendored/` au commit `references/mumble.pin`. Chaque module porte
//! des commentaires `// REF:` traçant chaque fait vers ces sources.
#![forbid(unsafe_code)]

pub mod framing;

pub use framing::{
    Frame, FramingError, HEADER_LEN, MAX_PAYLOAD_LEN, TcpMessageType, parse_frame, write_frame,
};
