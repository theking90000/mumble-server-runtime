//! Per-packet, per-recipient cost of the router (roadmap Phase 4, testing.md).
//!
//! Two measurements, deliberately separated, because they answer different
//! questions:
//!
//! - **lookup**: consulting the snapshot and deciding delivery. This is the part
//!   that must stay flat per recipient and allocation-free. It cannot allocate
//!   by construction rather than by discipline: `receivers` hands back a slice
//!   borrowed from the snapshot, and `AudioDecision` is a `Copy` struct with no
//!   heap field, so there is nothing for either to allocate. The compiler
//!   enforces what a runtime probe would only observe.
//! - **relay**: lookup plus building the outgoing envelope. This one *does*
//!   allocate once per recipient, and unavoidably: the Opus payload has to exist
//!   separately for each recipient because each is encrypted in its own OCB2
//!   domain (spec 15.2). Measuring it apart from the lookup is what keeps that
//!   cost visible instead of hidden inside a single blended number.
//!
//! Recipient counts span one listener to a crowded domain, so the shape of the
//! curve is visible: a regression that turns the per-packet path from linear in
//! recipients into something worse shows up as a bend, not as a slightly larger
//! number.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use voxloom_audio::{
    AudioTarget, Participant, RoutingDomainId, SessionId, compile, may_receive, outgoing_audio,
};
use voxloom_protocol::messages::udp;

/// Listener counts to measure. The speaker is extra, so a value of `n` means one
/// speaker routing to `n` recipients.
const RECIPIENTS: [usize; 4] = [1, 8, 32, 128];

/// A representative Opus frame. Real frames at 20 ms and a normal bitrate land
/// in this range; the payload size matters because the relay clones it.
const OPUS_FRAME: usize = 80;

fn snapshot_with(recipients: usize) -> (voxloom_audio::AudioRoutingSnapshot, SessionId) {
    let speaker = SessionId::new(0);
    let participants: Vec<Participant> = (0..=recipients)
        .map(|index| {
            Participant::new(
                SessionId::new(u32::try_from(index).unwrap_or(u32::MAX)),
                RoutingDomainId::DEFAULT,
            )
        })
        .collect();
    (compile(&participants, 1), speaker)
}

fn client_packet() -> udp::Audio {
    udp::Audio {
        header: Some(udp::audio::Header::Target(0)),
        sender_session: 0,
        frame_number: 1,
        opus_data: vec![0xA5; OPUS_FRAME],
        positional_data: Vec::new(),
        volume_adjustment: 0.0,
        is_terminator: false,
    }
}

fn bench_lookup(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("lookup");
    for recipients in RECIPIENTS {
        let (snapshot, speaker) = snapshot_with(recipients);
        group.throughput(Throughput::Elements(recipients as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(recipients),
            &recipients,
            |bencher, _| {
                bencher.iter(|| {
                    let found = snapshot.receivers(speaker, AudioTarget::Normal);
                    for receiver in found {
                        black_box(may_receive(
                            &snapshot,
                            speaker,
                            *receiver,
                            AudioTarget::Normal,
                        ));
                    }
                    black_box(found.len())
                });
            },
        );
    }
    group.finish();
}

fn bench_relay(criterion: &mut Criterion) {
    let source = client_packet();
    let mut group = criterion.benchmark_group("relay");
    for recipients in RECIPIENTS {
        let (snapshot, speaker) = snapshot_with(recipients);
        group.throughput(Throughput::Elements(recipients as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(recipients),
            &recipients,
            |bencher, _| {
                bencher.iter(|| {
                    for receiver in snapshot.receivers(speaker, AudioTarget::Normal) {
                        let decision =
                            may_receive(&snapshot, speaker, *receiver, AudioTarget::Normal);
                        black_box(outgoing_audio(&source, speaker, &decision));
                    }
                });
            },
        );
    }
    group.finish();
}

/// Compiling is the cold path and is allowed to be expensive, but it runs on
/// every join and leave, so a policy that made it quadratic *and* slow would
/// turn a busy lobby into a stall. Measured to keep that honest.
fn bench_compile(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("compile");
    for recipients in RECIPIENTS {
        let participants: Vec<Participant> = (0..=recipients)
            .map(|index| {
                Participant::new(
                    SessionId::new(u32::try_from(index).unwrap_or(u32::MAX)),
                    RoutingDomainId::DEFAULT,
                )
            })
            .collect();
        group.bench_with_input(
            BenchmarkId::from_parameter(recipients),
            &participants,
            |bencher, people| {
                bencher.iter(|| black_box(compile(people, 1)));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_lookup, bench_relay, bench_compile);
criterion_main!(benches);
