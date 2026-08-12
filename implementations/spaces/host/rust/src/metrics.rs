use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mumble_server_runtime_gateway::{RuntimeHandle, VoiceMetrics};
use mumble_server_runtime_shard::ReconcileReport;
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::task::JoinHandle;

#[derive(Debug, Clone)]
pub struct MetricsOutput {
    pub path: PathBuf,
    pub interval: Duration,
}

#[derive(Debug, Default)]
pub(crate) struct ActorMetrics {
    commands: AtomicU64,
    responses: AtomicU64,
    rejections: AtomicU64,
    resyncs: AtomicU64,
    lease_expirations: AtomicU64,
    queue_depth: AtomicUsize,
    queue_depth_max: AtomicUsize,
    queue_saturations: AtomicU64,
    sessions: AtomicUsize,
    participants: AtomicUsize,
    spaces: AtomicUsize,
    reconciliations: AtomicU64,
    reconciliation_total_nanos: AtomicU64,
    reconciliation_max_nanos: AtomicU64,
    view_delta_ops: AtomicU64,
    refused_publications: AtomicU64,
    routing_recompiles: AtomicU64,
}

impl ActorMetrics {
    pub(crate) fn command(&self, queue_depth: usize) {
        self.commands.fetch_add(1, Ordering::Relaxed);
        self.queue_depth.store(queue_depth, Ordering::Relaxed);
        self.queue_depth_max
            .fetch_max(queue_depth, Ordering::Relaxed);
    }

    pub(crate) fn gauges(&self, sessions: usize, participants: usize, spaces: usize) {
        self.sessions.store(sessions, Ordering::Relaxed);
        self.participants.store(participants, Ordering::Relaxed);
        self.spaces.store(spaces, Ordering::Relaxed);
    }

    pub(crate) fn response(&self) {
        self.responses.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn rejection(&self) {
        self.rejections.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn resync(&self) {
        self.resyncs.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn expired(&self, count: usize) {
        self.lease_expirations
            .fetch_add(u64::try_from(count).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    pub(crate) fn saturated(&self) {
        self.queue_saturations.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn reconciled(&self, elapsed: Duration, report: &ReconcileReport) {
        let nanos = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
        self.reconciliations.fetch_add(1, Ordering::Relaxed);
        self.reconciliation_total_nanos
            .fetch_add(nanos, Ordering::Relaxed);
        self.reconciliation_max_nanos
            .fetch_max(nanos, Ordering::Relaxed);
        self.view_delta_ops.fetch_add(
            u64::try_from(report.delta_len).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.refused_publications
            .fetch_add(u64::from(report.refused.is_some()), Ordering::Relaxed);
        self.routing_recompiles
            .fetch_add(u64::from(report.routing_recompiled), Ordering::Relaxed);
    }

    fn snapshot(&self) -> ActorSnapshot {
        ActorSnapshot {
            commands: self.commands.load(Ordering::Relaxed),
            responses: self.responses.load(Ordering::Relaxed),
            rejections: self.rejections.load(Ordering::Relaxed),
            resyncs: self.resyncs.load(Ordering::Relaxed),
            lease_expirations: self.lease_expirations.load(Ordering::Relaxed),
            queue_depth: self.queue_depth.load(Ordering::Relaxed),
            queue_depth_max: self.queue_depth_max.load(Ordering::Relaxed),
            queue_saturations: self.queue_saturations.load(Ordering::Relaxed),
            sessions: self.sessions.load(Ordering::Relaxed),
            participants: self.participants.load(Ordering::Relaxed),
            spaces: self.spaces.load(Ordering::Relaxed),
            reconciliations: self.reconciliations.load(Ordering::Relaxed),
            reconciliation_total_nanos: self.reconciliation_total_nanos.load(Ordering::Relaxed),
            reconciliation_max_nanos: self.reconciliation_max_nanos.load(Ordering::Relaxed),
            view_delta_ops: self.view_delta_ops.load(Ordering::Relaxed),
            refused_publications: self.refused_publications.load(Ordering::Relaxed),
            routing_recompiles: self.routing_recompiles.load(Ordering::Relaxed),
        }
    }
}

#[derive(Serialize)]
struct MetricsSnapshot {
    unix_millis: u64,
    actor: ActorSnapshot,
    shards: usize,
    connections: usize,
    audio_ingress_packets: u64,
    audio_ingress_bytes: u64,
    audio_egress_packets: u64,
    audio_egress_bytes: u64,
    audio_fanout_deliveries: u64,
    audio_dropped_packets: u64,
}

#[derive(Serialize)]
struct ActorSnapshot {
    commands: u64,
    responses: u64,
    rejections: u64,
    resyncs: u64,
    lease_expirations: u64,
    queue_depth: usize,
    queue_depth_max: usize,
    queue_saturations: u64,
    sessions: usize,
    participants: usize,
    spaces: usize,
    reconciliations: u64,
    reconciliation_total_nanos: u64,
    reconciliation_max_nanos: u64,
    view_delta_ops: u64,
    refused_publications: u64,
    routing_recompiles: u64,
}

pub(crate) fn spawn_writer(
    output: MetricsOutput,
    actor: Arc<ActorMetrics>,
    voice: Arc<VoiceMetrics>,
    runtime: RuntimeHandle,
) -> JoinHandle<Result<(), std::io::Error>> {
    tokio::spawn(async move {
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(output.path)
            .await?;
        let mut interval = tokio::time::interval(output.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let audio = voice.snapshot();
            let snapshot = MetricsSnapshot {
                unix_millis: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |duration| {
                        u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
                    }),
                actor: actor.snapshot(),
                shards: runtime.status().len(),
                connections: runtime.peers().len(),
                audio_ingress_packets: audio.ingress_packets,
                audio_ingress_bytes: audio.ingress_bytes,
                audio_egress_packets: audio.egress_packets,
                audio_egress_bytes: audio.egress_bytes,
                audio_fanout_deliveries: audio.egress_packets,
                audio_dropped_packets: audio.dropped_packets,
            };
            let mut encoded = serde_json::to_vec(&snapshot).map_err(std::io::Error::other)?;
            encoded.push(b'\n');
            file.write_all(&encoded).await?;
            file.flush().await?;
        }
    })
}
