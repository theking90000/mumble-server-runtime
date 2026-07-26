//! Deterministic no-panic smoke over the router, runnable on stable in the
//! normal `cargo test` CI. It is a cheap proxy for the `audio_route` cargo-fuzz
//! target under `fuzz/` (which needs nightly): routing must always return, never
//! panic, on arbitrary envelopes and arbitrary session identifiers.
//!
//! Both halves are covered because both take hostile input: the envelope fields
//! come straight off the wire, and the session identifiers come from whichever
//! connection proved ownership of a UDP address.

#![allow(clippy::expect_used)]

use voxloom_audio::{
    AudioTarget, Participant, RoutingDomainId, SessionId, compile, may_receive, outgoing_audio,
};
use voxloom_protocol::messages::udp;

/// xorshift64 — a tiny deterministic PRNG, so failures reproduce exactly.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn u32_below(&mut self, bound: u32) -> u32 {
        if bound == 0 {
            return 0;
        }
        u32::try_from(self.next_u64() % u64::from(bound)).unwrap_or(0)
    }

    fn bytes(&mut self, max_len: usize) -> Vec<u8> {
        let len = usize::try_from(self.next_u64()).unwrap_or(0) % (max_len + 1);
        (0..len)
            .map(|_| u8::try_from(self.next_u64() & 0xFF).unwrap_or(0))
            .collect()
    }
}

#[test]
fn routing_never_panics_on_arbitrary_input() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);

    for _ in 0..50_000 {
        // A randomly shaped participant set, including contradictory and empty
        // ones, so the compiler's fail-closed paths are exercised too.
        let population = rng.u32_below(6);
        let participants: Vec<Participant> = (0..population)
            .map(|_| {
                Participant::new(
                    SessionId::new(rng.u32_below(8)),
                    RoutingDomainId::new(rng.u32_below(3)),
                )
            })
            .collect();
        let snapshot = compile(&participants, rng.next_u64());

        let source = udp::Audio {
            header: Some(udp::audio::Header::Target(rng.u32_below(u32::MAX))),
            sender_session: rng.u32_below(u32::MAX),
            frame_number: rng.next_u64(),
            opus_data: rng.bytes(64),
            positional_data: Vec::new(),
            volume_adjustment: 0.0,
            is_terminator: false,
        };

        let target = AudioTarget::from_wire(rng.u32_below(40));
        // Senders deliberately include identifiers absent from the snapshot.
        let sender = SessionId::new(rng.u32_below(16));

        for receiver in snapshot.receivers(sender, target) {
            let decision = may_receive(&snapshot, sender, *receiver, target);
            let _ = outgoing_audio(&source, sender, &decision);
        }
        // And a direct probe with an unrelated receiver, which the recipient
        // list would never yield.
        let stranger = SessionId::new(rng.u32_below(64));
        let decision = may_receive(&snapshot, sender, stranger, target);
        let _ = outgoing_audio(&source, sender, &decision);
    }
}
