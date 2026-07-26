//! Target vocabulary (spec 15.5) and directional policy (spec 15.4).
//!
//! [`may_receive`] is the packet-dependent half of routing: the snapshot already
//! settled who *can* be reached, this settles who is reached by *this* packet
//! and how the outgoing envelope should describe it.

use crate::snapshot::{AudioRoutingSnapshot, SessionId};

/// Normal talking.
/// REF: references/vendored/MumbleUDP.proto : `Audio.target` — "0 means normal
///   talking".
const WIRE_NORMAL: u32 = 0;

/// The reserved server-loopback target.
/// REF: references/vendored/MumbleUDP.proto : `Audio.target` — "2^{5} - 1 means
///   server loopback".
const WIRE_SERVER_LOOPBACK: u32 = 31;

/// What a client asked the server to do with a voice packet.
///
/// REF: references/vendored/MumbleUDP.proto : `Audio.target` — "all other
///   targets are understood as shout/whisper targets that have previously been
///   registered via a VoiceTarget message (via TCP)".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioTarget {
    /// Speak to whoever the routing policy says can hear you.
    Normal,
    /// Ask the server to reflect the packet back to its sender.
    ServerLoopback,
    /// A shout or whisper target registered with a `VoiceTarget` message. Not
    /// accepted yet, kept as a distinct variant so refusing it is a decision
    /// rather than an accident.
    Registered(u32),
}

impl AudioTarget {
    pub const fn from_wire(raw: u32) -> Self {
        match raw {
            WIRE_NORMAL => Self::Normal,
            WIRE_SERVER_LOOPBACK => Self::ServerLoopback,
            other => Self::Registered(other),
        }
    }

    pub const fn to_wire(self) -> u32 {
        match self {
            Self::Normal => WIRE_NORMAL,
            Self::ServerLoopback => WIRE_SERVER_LOOPBACK,
            Self::Registered(raw) => raw,
        }
    }
}

/// How the server describes delivered audio to the receiving client.
///
/// REF: references/vendored/MumbleUDP.proto : `Audio.context` — "0: Normal
///   speech, 1: Shout to channel, 2: Whisper to user, 3: Received via channel
///   listener".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioContext {
    NormalSpeech,
    ShoutToChannel,
    WhisperToUser,
    ChannelListener,
}

impl AudioContext {
    pub const fn to_wire(self) -> u32 {
        match self {
            Self::NormalSpeech => 0,
            Self::ShoutToChannel => 1,
            Self::WhisperToUser => 2,
            Self::ChannelListener => 3,
        }
    }
}

/// The per-recipient answer for one packet (spec 15.4).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioDecision {
    /// Whether this recipient gets the packet at all.
    pub deliver: bool,
    /// Whether the sender's positional data may travel with it.
    pub include_position: bool,
    /// How the recipient should interpret the stream.
    pub context: AudioContext,
    /// A server-chosen gain, or `None` to leave it unset.
    pub volume_adjustment: Option<f32>,
}

impl AudioDecision {
    /// The refusal. Every path that cannot prove delivery is allowed returns
    /// this, so an unhandled case silently drops audio instead of leaking it.
    pub const fn deny() -> Self {
        Self {
            deliver: false,
            include_position: false,
            context: AudioContext::NormalSpeech,
            volume_adjustment: None,
        }
    }

    const fn deliver_as(context: AudioContext) -> Self {
        Self {
            deliver: true,
            include_position: true,
            context,
            volume_adjustment: None,
        }
    }
}

/// Decide whether `receiver` gets a packet `sender` addressed to `target`.
///
/// The domain check is deliberately redundant with the compiled table, which
/// already only lists same-domain recipients. Isolation between domains is the
/// one property that must never degrade quietly when the compiler grows a real
/// policy, so it is verified here too rather than assumed from the caller.
///
/// Phase 4 forwards positional data whenever it delivers, which is what the
/// Phase 3 loopback already did; deciding *when* position may travel is the
/// proximity work of Phase 8. No gain is applied, so `volume_adjustment` stays
/// unset.
pub fn may_receive(
    snapshot: &AudioRoutingSnapshot,
    sender: SessionId,
    receiver: SessionId,
    target: AudioTarget,
) -> AudioDecision {
    let (Some(sender_domain), Some(receiver_domain)) =
        (snapshot.domain_of(sender), snapshot.domain_of(receiver))
    else {
        // One of them is not in this snapshot generation. Refuse rather than
        // guess: a session that has not been compiled in has no policy yet.
        return AudioDecision::deny();
    };

    if sender_domain != receiver_domain {
        return AudioDecision::deny();
    }

    match target {
        // Normal speech never returns to its sender; the client plays back its
        // own voice locally, and echoing it would be an audible defect.
        AudioTarget::Normal if sender == receiver => AudioDecision::deny(),
        AudioTarget::Normal => AudioDecision::deliver_as(AudioContext::NormalSpeech),

        // Loopback is the exact opposite: only the sender may receive it.
        AudioTarget::ServerLoopback if sender == receiver => {
            AudioDecision::deliver_as(AudioContext::NormalSpeech)
        }
        AudioTarget::ServerLoopback => AudioDecision::deny(),

        // Registered shout/whisper targets need `VoiceTarget` handling.
        AudioTarget::Registered(_) => AudioDecision::deny(),
    }
}
