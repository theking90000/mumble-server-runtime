//! What one full publication costs.
//!
//! Spec 27.1 puts "full rerender of every connection as the oracle" third in
//! the priority list, right after correctness and the precompiled audio path,
//! and 27.4 forbids optimising it before measuring it. This binary is that
//! measurement: for a range of connection counts, it times the whole cold path
//! of a snapshot change — render every connection, validate the outputs, plan
//! the transitions, then commit them and republish the routing table.
//!
//! It measures the pure pipeline, no sockets: what a real server adds on top is
//! one queue push per connection, which the outbound queue already bounds.
//!
//! REF: docs/voxloom-roadmap-agents-v0_1.md P7 T8
//! REF: docs/voxloom-specification-technique-v0.1.md 27.1, 27.4

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use voxloom_control::{PublicationCoordinator, render_snapshot, validate_rendered_snapshot};
use voxloom_flavor::{ConnectionId, ServerPresentation, SnapshotSource, VoiceEvent, VoiceFlavor};
use voxloom_flavor_reference::{Realm, ReferenceFlavor};
use voxloom_render::SessionId;

/// Connection counts to measure. The upper end is well past what the roadmap
/// targets, so a superlinear cost shows up here rather than in production.
const SIZES: [u32; 5] = [2, 10, 50, 200, 500];

/// Generations timed per size. The reported figure is the median, which is what
/// a run under an unrelated system load should still report honestly.
const GENERATIONS: usize = 20;

fn main() -> Result<()> {
    println!(
        "{:>7}  {:>12}  {:>14}  {:>12}",
        "conns", "median", "per connection", "worst"
    );
    for size in SIZES {
        let samples = measure(size)?;
        let median = percentile(&samples, 50).context("no sample")?;
        let worst = samples.iter().max().copied().context("no sample")?;
        let per_connection = median / size.max(1);
        println!(
            "{size:>7}  {:>12}  {:>14}  {:>12}",
            format!("{median:.2?}"),
            format!("{per_connection:.2?}"),
            format!("{worst:.2?}"),
        );
    }
    Ok(())
}

/// Time `GENERATIONS` full publications of a snapshot change over `size`
/// connections. Each generation moves one member to the other realm, which is
/// the smallest business change that alters every other connection's view.
fn measure(size: u32) -> Result<Vec<Duration>> {
    let flavor = ReferenceFlavor::new("Voxloom", ServerPresentation::default());
    let mut coordinator = PublicationCoordinator::new();
    let mut connections = Vec::new();

    for index in 1..=size {
        let connection = ConnectionId::new(u64::from(index));
        flavor.observe(&VoiceEvent::Connected {
            connection,
            generation: 0,
            name: format!("member{index}@aurora"),
            certificate_hash: None,
        });
        coordinator
            .register(connection, SessionId(index))
            .map_err(|error| anyhow::anyhow!("registering connection {index}: {error}"))?;
        connections.push(connection);
    }

    // The first generation is the cold one (every view is created from nothing);
    // it is published outside the measurement so the samples describe a change,
    // not a first sync.
    publish(&flavor, &mut coordinator, &connections)?;

    let mut samples = Vec::with_capacity(GENERATIONS);
    for generation in 0..GENERATIONS {
        let mover = connections
            .get(generation % connections.len())
            .copied()
            .context("no connection to move")?;
        // Always move to the realm the member is not in: a request that changes
        // nothing would measure an empty generation instead of a real one.
        let realm = match flavor.snapshot().realm_of(mover) {
            Some(Realm::Aurora) => Realm::Borealis,
            _ => Realm::Aurora,
        };
        flavor.observe(&VoiceEvent::ChannelInteractionRequested {
            connection: mover,
            generation: 0,
            channel: realm.key(),
        });

        let started = Instant::now();
        publish(&flavor, &mut coordinator, &connections)?;
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    Ok(samples)
}

/// One complete publication: render, validate, plan, commit, republish.
fn publish(
    flavor: &ReferenceFlavor,
    coordinator: &mut PublicationCoordinator,
    connections: &[ConnectionId],
) -> Result<()> {
    let rendered = render_snapshot(flavor, flavor.snapshot(), connections.iter().copied())
        .map_err(|error| anyhow::anyhow!("render: {error}"))?;
    let validated = validate_rendered_snapshot(rendered)
        .map_err(|error| anyhow::anyhow!("validate: {error}"))?;
    let pending = coordinator
        .publish(&validated)
        .map_err(|error| anyhow::anyhow!("publish: {error}"))?;
    let (deliveries, commit) = pending.split();
    if deliveries.is_empty() {
        bail!("a snapshot change produced no view transition");
    }
    coordinator
        .commit(commit)
        .map_err(|error| anyhow::anyhow!("commit: {error}"))?;
    Ok(())
}

fn percentile(sorted: &[Duration], percent: usize) -> Option<Duration> {
    if sorted.is_empty() {
        return None;
    }
    let index = sorted.len().saturating_mul(percent) / 100;
    sorted.get(index.min(sorted.len() - 1)).copied()
}
