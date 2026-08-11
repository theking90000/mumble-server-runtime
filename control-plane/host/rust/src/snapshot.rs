use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::watch;

/// A complete immutable desired-state snapshot with a monotonic application revision.
pub trait VersionedSnapshot: Send + Sync + 'static {
    fn revision(&self) -> u64;
}

/// The actor-facing half of a latest-wins desired-state channel.
#[derive(Debug, Clone)]
pub struct SnapshotPublisher<T> {
    sender: watch::Sender<Arc<T>>,
    marker: PublicationMarker,
}

impl<T> SnapshotPublisher<T> {
    /// Replace the desired snapshot without queueing obsolete intermediate states.
    pub fn publish(&self, snapshot: Arc<T>) {
        self.sender.send_replace(snapshot);
    }

    /// A cheap marker suitable for a non-blocking runtime publication callback.
    #[must_use]
    pub fn publication_marker(&self) -> PublicationMarker {
        self.marker.clone()
    }
}

/// The renderer-facing half of a latest-wins desired-state channel.
#[derive(Debug)]
pub struct SnapshotReader<T> {
    receiver: watch::Receiver<Arc<T>>,
    marker: PublicationMarker,
}

impl<T> SnapshotReader<T> {
    /// Read the current desired snapshot without advancing publication correlation.
    #[must_use]
    pub fn current(&self) -> Arc<T> {
        Arc::clone(&self.receiver.borrow())
    }
}

impl<T: VersionedSnapshot> SnapshotReader<T> {
    /// Take the newest complete snapshot and mark its revision as rendered.
    pub fn latest(&mut self) -> Arc<T> {
        let snapshot = Arc::clone(&self.receiver.borrow_and_update());
        self.marker
            .rendered_revision
            .store(snapshot.revision(), Ordering::SeqCst);
        snapshot
    }
}

/// Correlates a runtime reconciliation report with the snapshot its render observed.
#[derive(Debug, Clone)]
pub struct PublicationMarker {
    rendered_revision: Arc<AtomicU64>,
}

impl PublicationMarker {
    #[must_use]
    pub fn rendered_revision(&self) -> u64 {
        self.rendered_revision.load(Ordering::SeqCst)
    }
}

/// Create a bounded latest-wins bridge from an actor to a synchronous renderer.
#[must_use]
pub fn snapshot_channel<T: VersionedSnapshot>(
    initial: Arc<T>,
) -> (SnapshotPublisher<T>, SnapshotReader<T>) {
    let (sender, receiver) = watch::channel(initial);
    let marker = PublicationMarker {
        rendered_revision: Arc::new(AtomicU64::new(0)),
    };
    (
        SnapshotPublisher {
            sender,
            marker: marker.clone(),
        },
        SnapshotReader { receiver, marker },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Snapshot(u64);

    impl VersionedSnapshot for Snapshot {
        fn revision(&self) -> u64 {
            self.0
        }
    }

    #[test]
    fn snapshots_are_coalesced_and_marked_only_when_rendered() {
        let (publisher, mut reader) = snapshot_channel(Arc::new(Snapshot(0)));
        let marker = publisher.publication_marker();
        publisher.publish(Arc::new(Snapshot(1)));
        publisher.publish(Arc::new(Snapshot(2)));

        assert_eq!(marker.rendered_revision(), 0);
        assert_eq!(reader.latest().0, 2);
        assert_eq!(marker.rendered_revision(), 2);
    }
}
