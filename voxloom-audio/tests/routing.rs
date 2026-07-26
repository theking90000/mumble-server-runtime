//! Property and unit tests for the pure router (spec 15.2/15.3/15.4/15.5).
//!
//! The properties that matter are the ones a real policy must never break when
//! it replaces the trivial Phase 4 one:
//!
//! - a speaker is never routed back to itself on normal speech, and *only* to
//!   itself on the loopback target;
//! - no packet ever crosses a routing domain, whichever way it is asked for;
//! - `may_receive` and the compiled recipient list agree, so the precompiled
//!   table can never deliver to someone the policy would refuse;
//! - the Opus payload survives the rewrite byte for byte.
//!
//! The generator is a seeded xorshift, printed on failure, rather than an
//! external property-testing dependency: it matches the convention already used
//! by the reconciler's planner tests and keeps the crate free of test-only
//! dependencies.

#![allow(clippy::expect_used)]

use std::collections::BTreeSet;

use voxloom_audio::{
    AudioContext, AudioDecision, AudioTarget, Participant, RoutingDomainId, SessionId, compile,
    may_receive, outgoing_audio,
};
use voxloom_protocol::messages::udp;

// ---------------------------------------------------------------------------
// Deterministic PRNG (xorshift64). Seeded, no external dependency; the seed is
// printed on failure so any counterexample is reproducible.
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        // Avoid the zero state, which xorshift cannot leave.
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15 | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, bound: u32) -> u32 {
        if bound == 0 {
            return 0;
        }
        u32::try_from(self.next() % u64::from(bound)).unwrap_or(0)
    }
}

/// Generate a participant set: a handful of sessions spread over a few domains,
/// with deliberate duplicate sessions so the compiler's collapsing is exercised.
fn participants(seed: u64) -> Vec<Participant> {
    let mut rng = Rng::new(seed);
    let count = rng.below(9) + 1;
    let domain_count = rng.below(3) + 1;

    (0..count)
        .map(|_| {
            // A small session space relative to the count guarantees collisions.
            let session = SessionId::new(rng.below(12));
            let domain = RoutingDomainId::new(rng.below(domain_count));
            Participant::new(session, domain)
        })
        .collect()
}

const SEEDS: u64 = 4000;

/// The sessions the snapshot actually routes. A session the generator declared
/// in two domains is deliberately absent (see
/// `a_session_declared_in_two_domains_is_routed_by_neither`), so properties
/// about routable sessions must ask the snapshot, not the raw input.
fn routable(
    snapshot: &voxloom_audio::AudioRoutingSnapshot,
    people: &[Participant],
) -> BTreeSet<SessionId> {
    people
        .iter()
        .map(|participant| participant.session)
        .filter(|session| snapshot.domain_of(*session).is_some())
        .collect()
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

#[test]
fn a_speaker_is_never_routed_back_to_itself() {
    for seed in 0..SEEDS {
        let people = participants(seed);
        let snapshot = compile(&people, seed);

        for participant in &people {
            let sender = participant.session;
            assert!(
                !snapshot
                    .receivers(sender, AudioTarget::Normal)
                    .contains(&sender),
                "seed {seed}: session {} routes normal speech to itself",
                sender.get()
            );
            assert!(
                !may_receive(&snapshot, sender, sender, AudioTarget::Normal).deliver,
                "seed {seed}: policy delivers normal speech from {} to itself",
                sender.get()
            );
        }
    }
}

#[test]
fn no_packet_crosses_a_routing_domain() {
    for seed in 0..SEEDS {
        let people = participants(seed);
        let snapshot = compile(&people, seed);

        for sender in routable(&snapshot, &people) {
            let sender_domain = snapshot
                .domain_of(sender)
                .expect("a routable session has a domain");

            for target in [AudioTarget::Normal, AudioTarget::ServerLoopback] {
                for receiver in snapshot.receivers(sender, target) {
                    let receiver_domain = snapshot
                        .domain_of(*receiver)
                        .expect("a routed recipient is in the snapshot");
                    assert_eq!(
                        sender_domain.get(),
                        receiver_domain.get(),
                        "seed {seed}: {} reaches {} across domains",
                        sender.get(),
                        receiver.get()
                    );
                }
            }

            // And the policy refuses it even when asked directly, which is what
            // keeps isolation from depending on the compiler alone.
            for other in &people {
                if snapshot.domain_of(other.session) != Some(sender_domain) {
                    assert!(
                        !may_receive(&snapshot, sender, other.session, AudioTarget::Normal).deliver,
                        "seed {seed}: policy delivers across domains"
                    );
                }
            }
        }
    }
}

#[test]
fn the_compiled_table_and_the_policy_agree() {
    for seed in 0..SEEDS {
        let people = participants(seed);
        let snapshot = compile(&people, seed);
        let sessions: BTreeSet<SessionId> = people.iter().map(|p| p.session).collect();

        for sender in &sessions {
            let routed: BTreeSet<SessionId> = snapshot
                .receivers(*sender, AudioTarget::Normal)
                .iter()
                .copied()
                .collect();

            for receiver in &sessions {
                let allowed =
                    may_receive(&snapshot, *sender, *receiver, AudioTarget::Normal).deliver;
                assert_eq!(
                    routed.contains(receiver),
                    allowed,
                    "seed {seed}: table and policy disagree for {} -> {}",
                    sender.get(),
                    receiver.get()
                );
            }
        }
    }
}

#[test]
fn loopback_reaches_the_sender_and_nobody_else() {
    for seed in 0..SEEDS {
        let people = participants(seed);
        let snapshot = compile(&people, seed);
        let sessions = routable(&snapshot, &people);

        for sender in &sessions {
            assert_eq!(
                snapshot.receivers(*sender, AudioTarget::ServerLoopback),
                &[*sender],
                "seed {seed}: loopback route is not exactly the sender"
            );

            for receiver in &sessions {
                let decision =
                    may_receive(&snapshot, *sender, *receiver, AudioTarget::ServerLoopback);
                assert_eq!(
                    decision.deliver,
                    sender == receiver,
                    "seed {seed}: loopback delivery from {} to {} is wrong",
                    sender.get(),
                    receiver.get()
                );
            }
        }
    }
}

#[test]
fn compiling_is_deterministic_regardless_of_input_order() {
    for seed in 0..SEEDS {
        let people = participants(seed);
        let reversed: Vec<Participant> = people.iter().rev().copied().collect();

        let forward = compile(&people, seed);
        let backward = compile(&reversed, seed);

        let sessions: BTreeSet<SessionId> = people.iter().map(|p| p.session).collect();
        for sender in &sessions {
            assert_eq!(
                forward.receivers(*sender, AudioTarget::Normal),
                backward.receivers(*sender, AudioTarget::Normal),
                "seed {seed}: input order changed the compiled routes"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Targets (spec 15.5)
// ---------------------------------------------------------------------------

#[test]
fn registered_targets_are_refused_rather_than_treated_as_normal_speech() {
    let alice = SessionId::new(1);
    let bob = SessionId::new(2);
    let snapshot = compile(
        &[
            Participant::new(alice, RoutingDomainId::DEFAULT),
            Participant::new(bob, RoutingDomainId::DEFAULT),
        ],
        1,
    );

    // Every target that is neither normal speech nor loopback needs a
    // `VoiceTarget` registration, which is not accepted yet.
    for raw in [1u32, 2, 30, 32, u32::MAX] {
        let target = AudioTarget::from_wire(raw);
        assert_eq!(target, AudioTarget::Registered(raw));
        assert!(
            snapshot.receivers(alice, target).is_empty(),
            "target {raw} produced a route"
        );
        assert!(
            !may_receive(&snapshot, alice, bob, target).deliver,
            "target {raw} was delivered"
        );
    }
}

#[test]
fn the_target_vocabulary_matches_the_wire() {
    assert_eq!(AudioTarget::from_wire(0), AudioTarget::Normal);
    assert_eq!(AudioTarget::from_wire(31), AudioTarget::ServerLoopback);
    assert_eq!(AudioTarget::Normal.to_wire(), 0);
    assert_eq!(AudioTarget::ServerLoopback.to_wire(), 31);
    assert_eq!(AudioTarget::Registered(7).to_wire(), 7);

    assert_eq!(AudioContext::NormalSpeech.to_wire(), 0);
    assert_eq!(AudioContext::ShoutToChannel.to_wire(), 1);
    assert_eq!(AudioContext::WhisperToUser.to_wire(), 2);
    assert_eq!(AudioContext::ChannelListener.to_wire(), 3);
}

#[test]
fn a_session_absent_from_the_snapshot_receives_nothing() {
    let known = SessionId::new(1);
    let stranger = SessionId::new(99);
    let snapshot = compile(&[Participant::new(known, RoutingDomainId::DEFAULT)], 1);

    assert!(snapshot.receivers(stranger, AudioTarget::Normal).is_empty());
    assert!(
        snapshot
            .receivers(stranger, AudioTarget::ServerLoopback)
            .is_empty()
    );
    assert!(!may_receive(&snapshot, known, stranger, AudioTarget::Normal).deliver);
    assert!(!may_receive(&snapshot, stranger, known, AudioTarget::Normal).deliver);
}

/// Found by `compiling_is_deterministic_regardless_of_input_order`: collapsing
/// duplicates by "first one wins" made the surviving domain depend on the input
/// ordering, so the same participants could compile to two different partitions
/// of the routes. A session claimed by two domains is now routed by neither.
#[test]
fn a_session_declared_in_two_domains_is_routed_by_neither() {
    let contested = SessionId::new(1);
    let alice = SessionId::new(2);
    let bob = SessionId::new(3);

    let snapshot = compile(
        &[
            Participant::new(contested, RoutingDomainId::new(0)),
            Participant::new(contested, RoutingDomainId::new(1)),
            Participant::new(alice, RoutingDomainId::new(0)),
            Participant::new(bob, RoutingDomainId::new(1)),
        ],
        1,
    );

    assert_eq!(snapshot.domain_of(contested), None, "no domain is assigned");
    assert!(
        snapshot
            .receivers(contested, AudioTarget::Normal)
            .is_empty(),
        "a contested session hears nobody"
    );
    for speaker in [alice, bob] {
        assert!(
            !snapshot
                .receivers(speaker, AudioTarget::Normal)
                .contains(&contested),
            "a contested session is heard by nobody"
        );
        assert!(!may_receive(&snapshot, speaker, contested, AudioTarget::Normal).deliver);
        assert!(!may_receive(&snapshot, contested, speaker, AudioTarget::Normal).deliver);
    }

    // Its own loopback is refused too: it has no policy at all, not a partial one.
    assert!(
        snapshot
            .receivers(contested, AudioTarget::ServerLoopback)
            .is_empty()
    );
}

#[test]
fn the_generation_is_carried_through() {
    let snapshot = compile(&[], 42);
    assert_eq!(snapshot.generation(), 42);
    assert!(snapshot.routes().is_empty());
}

// ---------------------------------------------------------------------------
// Envelope rewriting (spec 15.2)
// ---------------------------------------------------------------------------

fn client_packet() -> udp::Audio {
    udp::Audio {
        header: Some(udp::audio::Header::Target(0)),
        // A client is not required to set this, and the server must not trust it.
        sender_session: 12345,
        frame_number: 7,
        opus_data: vec![0xDE, 0xAD, 0xBE, 0xEF],
        positional_data: vec![1.0, 2.0, 3.0],
        volume_adjustment: 0.0,
        is_terminator: true,
    }
}

#[test]
fn the_outgoing_envelope_carries_the_payload_untouched() {
    let source = client_packet();
    let decision = AudioDecision {
        deliver: true,
        include_position: true,
        context: AudioContext::NormalSpeech,
        volume_adjustment: None,
    };

    let out = outgoing_audio(&source, SessionId::new(4), &decision).expect("delivered");

    assert_eq!(
        out.opus_data, source.opus_data,
        "Opus is forwarded verbatim"
    );
    assert_eq!(out.frame_number, source.frame_number);
    assert_eq!(out.is_terminator, source.is_terminator);
    assert_eq!(out.positional_data, source.positional_data);
}

#[test]
fn the_outgoing_envelope_replaces_target_with_context_and_stamps_the_sender() {
    let source = client_packet();
    let decision = AudioDecision {
        deliver: true,
        include_position: true,
        context: AudioContext::WhisperToUser,
        volume_adjustment: None,
    };

    let out = outgoing_audio(&source, SessionId::new(4), &decision).expect("delivered");

    // The client-claimed session is overwritten with the authenticated one.
    assert_eq!(out.sender_session, 4);
    assert_eq!(
        out.header,
        Some(udp::audio::Header::Context(
            AudioContext::WhisperToUser.to_wire()
        )),
        "a server-to-client packet carries context, never target"
    );
}

#[test]
fn position_is_dropped_when_the_decision_withholds_it() {
    let source = client_packet();
    let decision = AudioDecision {
        deliver: true,
        include_position: false,
        context: AudioContext::NormalSpeech,
        volume_adjustment: None,
    };

    let out = outgoing_audio(&source, SessionId::new(4), &decision).expect("delivered");
    assert!(out.positional_data.is_empty());
}

#[test]
fn an_unset_gain_is_encoded_as_zero() {
    let source = client_packet();
    let decision = AudioDecision {
        deliver: true,
        include_position: true,
        context: AudioContext::NormalSpeech,
        volume_adjustment: None,
    };

    // REF: MumbleUDP.proto — "A value of 0 means that this field is unset".
    let out = outgoing_audio(&source, SessionId::new(4), &decision).expect("delivered");
    assert_eq!(out.volume_adjustment, 0.0);

    let loud = AudioDecision {
        volume_adjustment: Some(0.5),
        ..decision
    };
    let out = outgoing_audio(&source, SessionId::new(4), &loud).expect("delivered");
    assert_eq!(out.volume_adjustment, 0.5);
}

#[test]
fn a_refused_decision_produces_no_envelope() {
    let source = client_packet();
    assert!(outgoing_audio(&source, SessionId::new(4), &AudioDecision::deny()).is_none());
}
