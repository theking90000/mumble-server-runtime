//! Rewriting the outgoing voice envelope (spec 15.2).
//!
//! Routing a packet means keeping the Opus payload, rewriting the metadata the
//! recipient needs, and letting the session layer encrypt the result separately
//! for each recipient. The payload is never decoded: decoding is only required
//! for mixing, server-side DSP, transcoding or content analysis, none of which
//! Voxloom does.

use voxloom_protocol::messages::udp;

use crate::policy::AudioDecision;
use crate::snapshot::SessionId;

/// Build the server-to-client form of a client-sent voice packet, or `None` if
/// the decision refuses delivery.
///
/// Returning an `Option` rather than trusting the caller to check `deliver`
/// first is deliberate: it makes "build an envelope for a recipient who was
/// denied" unrepresentable instead of merely discouraged.
///
/// REF: references/vendored/MumbleUDP.proto : the `Header` oneof carries
///   `target` client-to-server and `context` server-to-client, so the target
///   must be replaced rather than forwarded; `sender_session` "will always be
///   set when receiving audio from the server"; `volume_adjustment` "a value of
///   0 means that this field is unset".
pub fn outgoing_audio(
    source: &udp::Audio,
    sender: SessionId,
    decision: &AudioDecision,
) -> Option<udp::Audio> {
    if !decision.deliver {
        return None;
    }

    Some(udp::Audio {
        header: Some(udp::audio::Header::Context(decision.context.to_wire())),
        sender_session: sender.get(),
        frame_number: source.frame_number,
        // The encoded frames travel untouched: this is the whole point of not
        // decoding Opus to route.
        opus_data: source.opus_data.clone(),
        positional_data: if decision.include_position {
            source.positional_data.clone()
        } else {
            Vec::new()
        },
        volume_adjustment: decision.volume_adjustment.unwrap_or(0.0),
        is_terminator: source.is_terminator,
    })
}
