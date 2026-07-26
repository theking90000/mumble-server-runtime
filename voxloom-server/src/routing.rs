//! Applying the pure router's answers to real connections.
//!
//! This is the seam between the two worlds. Everything upstream of it is a pure
//! function over an immutable snapshot; everything downstream owns cryptographic
//! state and sockets. Keeping the seam this thin is what lets the router itself
//! stay free of locks and awaits (ADR-005).
//!
//! Both ingress paths converge here, which is the point: a packet that arrives
//! over UDP and one that arrives through the TCP tunnel are routed by the same
//! code, and each recipient is reached over whichever transport *it* last used.
//! Cross-transport delivery therefore needs no special case.

use std::net::SocketAddr;

use voxloom_audio::{AudioTarget, SessionId, may_receive, outgoing_audio};
use voxloom_protocol::messages::udp;
use voxloom_protocol::{ControlMessage, UdpMessage, encode_udp};

use crate::outbound::VoiceAdmission;
use crate::state::SharedState;
use crate::voice::encrypt;

/// A sealed datagram and where to send it.
pub type Datagram = (Vec<u8>, SocketAddr);

/// Route one voice packet to every recipient the snapshot allows.
///
/// Returns the datagrams the caller must put on the UDP socket. Recipients that
/// are on the TCP tunnel are served inline by pushing onto their connection's
/// outbound queue, because that queue is synchronous and needs no socket.
///
/// The snapshot is read once for the whole packet: every recipient of a given
/// datagram is decided against the same generation, so a membership change in
/// the middle cannot deliver half a packet under the old policy and half under
/// the new one.
pub fn deliver_audio(
    state: &SharedState,
    sender_session: u32,
    audio: &udp::Audio,
    target: AudioTarget,
) -> Vec<Datagram> {
    let snapshot = state.routing_snapshot();
    let sender = SessionId::new(sender_session);
    let recipients = snapshot.receivers(sender, target);
    if recipients.is_empty() {
        return Vec::new();
    }

    let mut datagrams = Vec::new();
    for entry in state.users_for(recipients) {
        let receiver = SessionId::new(entry.session);
        let decision = may_receive(&snapshot, sender, receiver, target);
        let Some(envelope) = outgoing_audio(audio, sender, &decision) else {
            continue;
        };
        let plaintext = encode_udp(&UdpMessage::Audio(envelope));

        match entry.udp_destination() {
            Some(addr) => match encrypt(&entry, &plaintext) {
                Some(sealed) => datagrams.push((sealed, addr)),
                None => {
                    // No usable crypto state: the connection is mid-teardown or
                    // never completed setup. Dropping is the only safe outcome;
                    // sending plaintext voice would be worse than silence.
                    eprintln!(
                        "voxloom-server: dropping audio for session {}: no crypto state",
                        entry.session
                    );
                }
            },
            None => {
                // TCP tunnel fallback (spec 15.6). The queue is bounded, so this
                // is an admission decision, not an unconditional push.
                match entry
                    .outbound
                    .push_voice(ControlMessage::UdpTunnel(plaintext))
                {
                    VoiceAdmission::Accepted => {}
                    // Counted and logged by the queue. A gap is the right
                    // outcome for a recipient already behind: stale voice helps
                    // nobody, and refusing it here keeps the same connection
                    // healthy for the control traffic that still matters.
                    VoiceAdmission::Dropped => {}
                }
            }
        }
    }

    datagrams
}

/// Read the target a client put on a voice packet.
///
/// A client-sent packet carries `target`; `context` is the server-to-client
/// direction. A client that sends `context`, or no header at all, is either
/// broken or probing, so it gets no route.
///
/// REF: references/vendored/MumbleUDP.proto : the `Header` oneof.
pub fn client_target(audio: &udp::Audio) -> Option<AudioTarget> {
    match audio.header {
        Some(udp::audio::Header::Target(raw)) => Some(AudioTarget::from_wire(raw)),
        _ => None,
    }
}
