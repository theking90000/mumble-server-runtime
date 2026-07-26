//! Pure control-plane coordination for flavor snapshot renders.
//!
//! T2 renders one immutable snapshot for every currently known connection. It
//! deliberately stops before output validation, transition planning, or
//! publication, which belong to later P7 tasks.
//!
//! REF: docs/voxloom-roadmap-agents-v0_1.md P7 T2
//! REF: docs/voxloom-specification-technique-v0.1.md 23.1, 24.1
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use voxloom_flavor::{ConnectionId, FlavorError, FlavorRevision, RenderOutput, VoiceFlavor};

mod validation;

pub use validation::{
    AudioRouteValidationError, DesiredViewValidationError, FlavorOutputValidationError,
    InteractionRegistryValidationError, ValidatedSnapshot, validate_rendered_snapshot,
};

/// Complete render result for one immutable flavor snapshot.
///
/// Keeping the `Arc` alongside the outputs makes the exact source snapshot
/// explicit. Later tasks can validate and stage this value without consulting
/// mutable business state.
#[derive(Debug)]
#[must_use = "a rendered snapshot has no effect until a later publication stage consumes it"]
pub struct RenderedSnapshot<S> {
    snapshot: Arc<S>,
    flavor_revision: FlavorRevision,
    outputs: BTreeMap<ConnectionId, RenderOutput>,
}

impl<S> RenderedSnapshot<S> {
    #[must_use]
    pub fn snapshot(&self) -> &Arc<S> {
        &self.snapshot
    }

    #[must_use]
    pub const fn flavor_revision(&self) -> FlavorRevision {
        self.flavor_revision
    }

    #[must_use]
    pub fn outputs(&self) -> &BTreeMap<ConnectionId, RenderOutput> {
        &self.outputs
    }
}

/// Render the same immutable snapshot for every distinct connection.
///
/// Connections are deduplicated and ordered before rendering, making the
/// result deterministic for any input order. A flavor error returns no
/// [`RenderedSnapshot`], so callers cannot mistake a partial render for a
/// complete candidate.
pub fn render_snapshot<F>(
    flavor: &F,
    snapshot: Arc<F::Snapshot>,
    connections: impl IntoIterator<Item = ConnectionId>,
) -> Result<RenderedSnapshot<F::Snapshot>, FlavorError>
where
    F: VoiceFlavor,
{
    let flavor_revision = flavor.revision(&snapshot);
    let connections: BTreeSet<ConnectionId> = connections.into_iter().collect();
    let mut outputs = BTreeMap::new();

    for connection in connections {
        let output = flavor.render(&snapshot, connection)?;
        outputs.insert(connection, output);
    }

    Ok(RenderedSnapshot {
        snapshot,
        flavor_revision,
        outputs,
    })
}

#[cfg(test)]
mod snapshot_publication {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use voxloom_flavor::{DesiredClientView, InteractionRegistry, RenderOutput};

    use super::*;

    #[derive(Debug)]
    struct Snapshot {
        revision: FlavorRevision,
        revision_reads: AtomicUsize,
        render_calls: AtomicUsize,
    }

    #[derive(Debug)]
    struct RecordingFlavor {
        refused_connection: Option<ConnectionId>,
    }

    impl VoiceFlavor for RecordingFlavor {
        type Snapshot = Snapshot;

        fn revision(&self, snapshot: &Self::Snapshot) -> FlavorRevision {
            snapshot.revision_reads.fetch_add(1, Ordering::Relaxed);
            snapshot.revision
        }

        fn render(
            &self,
            snapshot: &Self::Snapshot,
            connection: ConnectionId,
        ) -> Result<RenderOutput, FlavorError> {
            snapshot.render_calls.fetch_add(1, Ordering::Relaxed);
            if self.refused_connection == Some(connection) {
                return Err(FlavorError::render_refused(
                    connection,
                    "connection has no declarative output",
                ));
            }

            Ok(RenderOutput::new(
                DesiredClientView::empty(),
                BTreeSet::new(),
                InteractionRegistry::default(),
            ))
        }
    }

    fn snapshot(revision: u64) -> Arc<Snapshot> {
        Arc::new(Snapshot {
            revision: FlavorRevision::new(revision),
            revision_reads: AtomicUsize::new(0),
            render_calls: AtomicUsize::new(0),
        })
    }

    #[test]
    fn snapshot_publication_renders_every_distinct_connection_once() {
        let flavor = RecordingFlavor {
            refused_connection: None,
        };
        let snapshot = snapshot(12);
        let connections = [
            ConnectionId::new(3),
            ConnectionId::new(1),
            ConnectionId::new(3),
            ConnectionId::new(2),
        ];
        let result = render_snapshot(&flavor, Arc::clone(&snapshot), connections);

        match result {
            Ok(rendered) => {
                assert!(Arc::ptr_eq(rendered.snapshot(), &snapshot));
                assert_eq!(rendered.flavor_revision(), FlavorRevision::new(12));
                assert_eq!(
                    rendered.outputs().keys().copied().collect::<Vec<_>>(),
                    vec![
                        ConnectionId::new(1),
                        ConnectionId::new(2),
                        ConnectionId::new(3)
                    ]
                );
                assert_eq!(snapshot.revision_reads.load(Ordering::Relaxed), 1);
                assert_eq!(snapshot.render_calls.load(Ordering::Relaxed), 3);
            }
            Err(error) => panic!("snapshot render unexpectedly failed: {error}"),
        }
    }

    #[test]
    fn snapshot_publication_returns_no_partial_candidate_after_a_render_error() {
        let refused = ConnectionId::new(2);
        let flavor = RecordingFlavor {
            refused_connection: Some(refused),
        };
        let snapshot = snapshot(4);
        let connections = [ConnectionId::new(1), refused, ConnectionId::new(3)];
        let result = render_snapshot(&flavor, Arc::clone(&snapshot), connections);

        match result {
            Err(error) => {
                assert_eq!(error.connection(), refused);
                assert_eq!(snapshot.revision_reads.load(Ordering::Relaxed), 1);
                assert_eq!(snapshot.render_calls.load(Ordering::Relaxed), 2);
            }
            Ok(_) => panic!("partial snapshot render unexpectedly succeeded"),
        }
    }

    #[test]
    fn snapshot_publication_handles_an_empty_connection_set() {
        let flavor = RecordingFlavor {
            refused_connection: None,
        };
        let snapshot = snapshot(8);
        let result = render_snapshot(
            &flavor,
            Arc::clone(&snapshot),
            std::iter::empty::<ConnectionId>(),
        );

        match result {
            Ok(rendered) => {
                assert!(rendered.outputs().is_empty());
                assert!(Arc::ptr_eq(rendered.snapshot(), &snapshot));
                assert_eq!(snapshot.revision_reads.load(Ordering::Relaxed), 1);
                assert_eq!(snapshot.render_calls.load(Ordering::Relaxed), 0);
            }
            Err(error) => panic!("empty snapshot render unexpectedly failed: {error}"),
        }
    }
}
