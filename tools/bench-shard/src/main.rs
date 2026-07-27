//! What one shard turn costs.
//!
//! The deliberate counterpart of `bench-publication`: same business change (move
//! one member to the other realm), same connection counts, same statistic. The
//! two tables are meant to be read side by side, because the whole argument for
//! the shard model is a claim about how they differ as N grows.
//!
//! A turn here is the complete cold path of a state change: render the shard
//! once, plan the shared delta once, journal it, republish the audio routing
//! table, then for every connection filter that delta, splice its overlay,
//! collapse, encode and push.
//!
//! Draining the queues is a connection task's work, not the shard's, so it
//! happens outside the timed section - exactly as `bench-publication` measures
//! the pipeline and not the socket.
//!
//! REF: docs/design/guide-implementation.md 13 (the cost law)

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use voxloom_protocol::ControlMessage;
use voxloom_shard::{
    ChannelKey, ConnectionId, DomainId, Narrow, Occupant, OutboundQueue, Reply, Scope, ScopeSet,
    Shard, ShardBuilder, ShardCommand, ShardId, ShardLogic, VoiceEvent,
};

/// Connection counts to measure, identical to `bench-publication` so the two
/// tables line up row for row.
const SIZES: [u32; 5] = [2, 10, 50, 200, 500];

/// Turns timed per size. The reported figure is the median, so a run under an
/// unrelated system load still reports honestly.
const GENERATIONS: usize = 20;

/// Queue depth. Large enough that the initial sync of the biggest size fits,
/// since congestion would measure backpressure rather than the pipeline.
const QUEUE: usize = 4096;

fn main() -> Result<()> {
    println!(
        "{:>7}  {:>12}  {:>14}  {:>12}  {:>9}",
        "conns", "median", "per connection", "worst", "delta ops"
    );
    for size in SIZES {
        let (samples, delta) = measure(size)?;
        let median = percentile(&samples, 50).context("no sample")?;
        let worst = samples.iter().max().copied().context("no sample")?;
        let per_connection = median / size.max(1);
        println!(
            "{size:>7}  {:>12}  {:>14}  {:>12}  {delta:>9}",
            format!("{median:.2?}"),
            format!("{per_connection:.2?}"),
            format!("{worst:.2?}"),
        );
    }
    Ok(())
}

/// Time `GENERATIONS` shard turns over `size` connections, each turn moving one
/// member to the other realm.
///
/// Returns the samples and the size of the last shared delta, which is the
/// number the cost law is really about: it stays small while N grows.
fn measure(size: u32) -> Result<(Vec<Duration>, usize)> {
    let world = World {
        realms: (1..=size)
            .map(|index| (ConnectionId(u64::from(index)), index % 2))
            .collect(),
        label: 0,
    };
    let mut shard = Shard::new(ShardId(1), Realms { world });
    let mut receivers = Vec::new();

    for index in 1..=size {
        let (queue, receiver) = OutboundQueue::with_capacity(QUEUE);
        shard.handle(ShardCommand::attach(
            ConnectionId(u64::from(index)),
            Arc::new(queue),
        ));
        receivers.push(receiver);
    }

    // The initial sync is the slow path for everyone by construction; the steady
    // state is what this benchmark is about, so it is not timed.
    let report = shard.reconcile();
    if let Some(refused) = report.refused {
        bail!("the initial render was refused: {refused}");
    }
    drain(&mut receivers);

    let mut samples = Vec::with_capacity(GENERATIONS);
    let mut delta = 0;
    for generation in 0..GENERATIONS {
        let victim = ConnectionId(u64::from(
            u32::try_from(generation % usize::try_from(size)?)? + 1,
        ));
        let realm = u32::try_from(generation % 2)?;
        shard.logic_mut().world.realms.insert(victim, realm);

        let start = Instant::now();
        let report = shard.reconcile();
        samples.push(start.elapsed());

        if let Some(refused) = report.refused {
            bail!("generation {generation} was refused: {refused}");
        }
        if !report.closed.is_empty() {
            bail!("generation {generation} closed {:?}", report.closed);
        }
        if report.published {
            delta = report.delta_len;
        }
        drain(&mut receivers);
    }

    samples.sort_unstable();
    Ok((samples, delta))
}

fn drain(receivers: &mut [tokio::sync::mpsc::Receiver<ControlMessage>]) {
    for receiver in receivers {
        while receiver.try_recv().is_ok() {}
    }
}

fn percentile(sorted: &[Duration], percent: usize) -> Option<Duration> {
    if sorted.is_empty() {
        return None;
    }
    let index = sorted.len().saturating_sub(1) * percent / 100;
    sorted.get(index).copied()
}

// ---------------------------------------------------------------------------
// The same two-realm world bench-publication uses
// ---------------------------------------------------------------------------

struct World {
    realms: BTreeMap<ConnectionId, u32>,
    label: u32,
}

struct Realms {
    world: World,
}

impl ShardLogic for Realms {
    fn render(&mut self, out: &mut ShardBuilder<'_>) {
        let root = out.root("Voxloom");
        let label = self.world.label;

        for realm in 0..2u32 {
            let channel = out.channel(
                root,
                ChannelKey(u64::from(100 + realm)),
                &format!("Realm {realm} {label}"),
                Narrow::Into(realm),
            );
            let members: Vec<ConnectionId> = self
                .world
                .realms
                .iter()
                .filter(|(_, member_realm)| **member_realm == realm)
                .map(|(connection, _)| *connection)
                .collect();

            for member in &members {
                out.user(
                    channel,
                    Occupant::Connection(*member),
                    &format!("member-{}", member.0),
                    Narrow::Same,
                );
            }
            if members.len() > 1 {
                out.audio_domain(DomainId(u64::from(realm)), &members);
            }
        }
    }

    fn observation(&mut self, connection: ConnectionId) -> ScopeSet {
        self.world
            .realms
            .get(&connection)
            .and_then(|realm| Scope::ROOT.child(*realm))
            .and_then(|scope| ScopeSet::new(&[scope]).ok())
            .unwrap_or(ScopeSet::NONE)
    }

    fn observe(&mut self, _event: &VoiceEvent, _out: &mut Reply) {}
}
