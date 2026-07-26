//! Fuzz the pure router end to end: an arbitrary datagram is decoded, its target
//! is interpreted, and every recipient the snapshot yields gets an outgoing
//! envelope built for it.
//!
//! The contract is the same as every other target here: no panic, whatever the
//! bytes. The router runs on attacker-controlled envelopes (target, frame
//! number, positional array, Opus payload) once the sender is authenticated, so
//! a panic in this path takes the whole voice plane down with it.

#![no_main]

use libfuzzer_sys::fuzz_target;
use voxloom_audio::{
    AudioTarget, Participant, RoutingDomainId, SessionId, compile, may_receive, outgoing_audio,
};
use voxloom_protocol::{UdpMessage, decode_udp};

fuzz_target!(|data: &[u8]| {
    // Two peers in one domain and a third in another: enough to exercise
    // delivery, self-exclusion, loopback and the cross-domain refusal.
    let alice = SessionId::new(1);
    let bob = SessionId::new(2);
    let carol = SessionId::new(3);
    let snapshot = compile(
        &[
            Participant::new(alice, RoutingDomainId::new(0)),
            Participant::new(bob, RoutingDomainId::new(0)),
            Participant::new(carol, RoutingDomainId::new(1)),
        ],
        1,
    );

    let Ok(UdpMessage::Audio(audio)) = decode_udp(data) else {
        return;
    };

    // A client-sent packet carries a target; anything else is not routable.
    let raw_target = match audio.header {
        Some(voxloom_protocol::messages::udp::audio::Header::Target(target)) => target,
        _ => return,
    };
    let target = AudioTarget::from_wire(raw_target);

    for sender in [alice, bob, carol, SessionId::new(u32::MAX)] {
        for receiver in snapshot.receivers(sender, target) {
            let decision = may_receive(&snapshot, sender, *receiver, target);
            let _ = outgoing_audio(&audio, sender, &decision);
        }
    }
});
