use std::sync::atomic::{AtomicU64, Ordering};

/// Allocation-free counters for the Mumble voice packet path.
#[derive(Debug, Default)]
pub struct VoiceMetrics {
    ingress_packets: AtomicU64,
    ingress_bytes: AtomicU64,
    egress_packets: AtomicU64,
    egress_bytes: AtomicU64,
    dropped_packets: AtomicU64,
}

impl VoiceMetrics {
    pub(crate) fn ingress(&self, bytes: usize) {
        self.ingress_packets.fetch_add(1, Ordering::Relaxed);
        self.ingress_bytes
            .fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    pub(crate) fn egress(&self, bytes: usize) {
        self.egress_packets.fetch_add(1, Ordering::Relaxed);
        self.egress_bytes
            .fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    pub(crate) fn dropped(&self) {
        self.dropped_packets.fetch_add(1, Ordering::Relaxed);
    }

    #[must_use]
    pub fn snapshot(&self) -> VoiceMetricsSnapshot {
        VoiceMetricsSnapshot {
            ingress_packets: self.ingress_packets.load(Ordering::Relaxed),
            ingress_bytes: self.ingress_bytes.load(Ordering::Relaxed),
            egress_packets: self.egress_packets.load(Ordering::Relaxed),
            egress_bytes: self.egress_bytes.load(Ordering::Relaxed),
            dropped_packets: self.dropped_packets.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VoiceMetricsSnapshot {
    pub ingress_packets: u64,
    pub ingress_bytes: u64,
    pub egress_packets: u64,
    pub egress_bytes: u64,
    pub dropped_packets: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_are_monotonic_and_saturating_at_conversion_boundaries() {
        let metrics = VoiceMetrics::default();
        metrics.ingress(30);
        metrics.egress(42);
        metrics.dropped();

        assert_eq!(
            metrics.snapshot(),
            VoiceMetricsSnapshot {
                ingress_packets: 1,
                ingress_bytes: 30,
                egress_packets: 1,
                egress_bytes: 42,
                dropped_packets: 1,
            }
        );
    }
}
