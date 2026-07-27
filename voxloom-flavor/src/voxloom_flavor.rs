//! Static integration contract between Voxloom and a compiled flavor.
//!
//! A flavor owns its snapshot and every rule that interprets it. Voxloom only
//! supplies a stable voice-connection identity and consumes declarative,
//! key-based view and audio outputs. The snapshot remains opaque, immutable
//! during a render, and independent from the Mumble wire format.
//!
//! REF: docs/decisions/0002-flavor-owns-business-state.md
//! REF: docs/voxloom-specification-technique-v0.1.md 5.1, 5.4, 24.1
#![forbid(unsafe_code)]

use std::sync::Arc;

use thiserror::Error;

mod event;
mod output;

pub use event::VoiceEvent;
pub use output::{
    DesiredAudioRoute, DesiredChannel, DesiredClientView, DesiredListenerRelation, DesiredUser,
    InteractionRegistry, RenderOutput,
};
pub use voxloom_render::{
    ActionKey, ActionTarget, BlobRef, ChannelKey, ContextActionView, PermissionBits, SemanticKey,
    ServerPresentation, UserKey,
};

/// Stable runtime identity of one voice connection.
///
/// This is neither a wire session id nor a business identifier. The runtime
/// owns the mapping from this value to its connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnectionId(u64);

impl ConnectionId {
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Revision assigned by a flavor to one immutable snapshot.
///
/// Voxloom carries this value for correlation. It does not infer business
/// ordering from it or mutate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FlavorRevision(u64);

impl FlavorRevision {
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A flavor refused to produce outputs for a connection.
///
/// The caller must keep the last committed generation when this error is
/// returned. Publication and rollback mechanics belong to later P7 tasks.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum FlavorError {
    #[error("flavor refused to render connection {connection:?}: {reason}")]
    RenderRefused {
        connection: ConnectionId,
        reason: String,
    },
}

impl FlavorError {
    #[must_use]
    pub fn render_refused(connection: ConnectionId, reason: impl Into<String>) -> Self {
        Self::RenderRefused {
            connection,
            reason: reason.into(),
        }
    }

    #[must_use]
    pub const fn connection(&self) -> ConnectionId {
        match self {
            Self::RenderRefused { connection, .. } => *connection,
        }
    }

    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            Self::RenderRefused { reason, .. } => reason,
        }
    }
}

/// A statically compiled provider of declarative voice outputs.
///
/// Implementations own their snapshot type. Voxloom only borrows a snapshot
/// for the duration of [`VoiceFlavor::revision`] or [`VoiceFlavor::render`].
pub trait VoiceFlavor: Send + Sync + 'static {
    type Snapshot: Send + Sync + 'static;

    fn revision(&self, snapshot: &Self::Snapshot) -> FlavorRevision;

    fn render(
        &self,
        snapshot: &Self::Snapshot,
        connection: ConnectionId,
    ) -> Result<RenderOutput, FlavorError>;

    /// Observe one voice event.
    ///
    /// The flavor owns every business mutation and its own concurrency model,
    /// which is why this takes `&self`: Voxloom holds no lock on the flavor and
    /// applies nothing itself. Publishing a new snapshot afterwards is the
    /// flavor's own decision and its own call; ignoring the event is a valid
    /// business answer, so it is stated by an empty body rather than assumed.
    fn observe(&self, event: &VoiceEvent);
}

/// A flavor the runtime may pull the current snapshot from.
///
/// Publication is push-shaped: a flavor hands over a snapshot when it decides
/// to. A runtime that has just admitted a connection cannot wait for that
/// decision to render it, so this is the one way it may ask, and the answer is
/// still an immutable value the flavor produced.
pub trait SnapshotSource: VoiceFlavor {
    fn snapshot(&self) -> Arc<Self::Snapshot>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[derive(Debug)]
    struct Snapshot {
        revision: FlavorRevision,
        refuse_render: bool,
    }

    #[derive(Debug)]
    struct ExampleFlavor;

    impl VoiceFlavor for ExampleFlavor {
        type Snapshot = Snapshot;

        fn revision(&self, snapshot: &Self::Snapshot) -> FlavorRevision {
            snapshot.revision
        }

        fn render(
            &self,
            snapshot: &Self::Snapshot,
            connection: ConnectionId,
        ) -> Result<RenderOutput, FlavorError> {
            if snapshot.refuse_render {
                return Err(FlavorError::render_refused(
                    connection,
                    "snapshot cannot produce a voice output",
                ));
            }

            let routes = BTreeSet::from([DesiredAudioRoute {
                sender: ConnectionId::new(8),
                receiver: connection,
            }]);
            Ok(RenderOutput::new(
                DesiredClientView::empty(),
                routes,
                InteractionRegistry::default(),
            ))
        }

        fn observe(&self, _event: &VoiceEvent) {}
    }

    fn assert_contract<T: VoiceFlavor>() {}

    #[test]
    fn contract_accepts_a_static_send_sync_implementation() {
        assert_contract::<ExampleFlavor>();
    }

    #[test]
    fn revision_is_read_from_the_opaque_snapshot() {
        let flavor = ExampleFlavor;
        let snapshot = Snapshot {
            revision: FlavorRevision::new(41),
            refuse_render: false,
        };

        assert_eq!(flavor.revision(&snapshot), FlavorRevision::new(41));
    }

    #[test]
    fn render_returns_the_declared_view_and_routes() {
        let flavor = ExampleFlavor;
        let snapshot = Snapshot {
            revision: FlavorRevision::new(1),
            refuse_render: false,
        };
        let result = flavor.render(&snapshot, ConnectionId::new(7));

        match result {
            Ok(output) => {
                assert_eq!(output.client_view(), &DesiredClientView::empty());
                assert_eq!(
                    output.audio_routes(),
                    &BTreeSet::from([DesiredAudioRoute {
                        sender: ConnectionId::new(8),
                        receiver: ConnectionId::new(7),
                    }])
                );
                assert_eq!(output.interactions(), &InteractionRegistry::default());
                let (view, routes, interactions) = output.into_parts();
                assert_eq!(view, DesiredClientView::empty());
                assert_eq!(routes.len(), 1);
                assert_eq!(interactions, InteractionRegistry::default());
            }
            Err(error) => panic!("render unexpectedly failed: {error}"),
        }
    }

    #[test]
    fn render_error_keeps_connection_context_and_reason() {
        let flavor = ExampleFlavor;
        let snapshot = Snapshot {
            revision: FlavorRevision::new(2),
            refuse_render: true,
        };
        let connection = ConnectionId::new(9);
        let result = flavor.render(&snapshot, connection);

        match result {
            Err(error) => {
                assert_eq!(error.connection(), connection);
                assert_eq!(error.reason(), "snapshot cannot produce a voice output");
            }
            Ok(_) => panic!("render unexpectedly succeeded"),
        }
    }

    #[test]
    fn identifiers_roundtrip_without_domain_meaning() {
        assert_eq!(ConnectionId::new(17).get(), 17);
        assert_eq!(FlavorRevision::new(23).get(), 23);
    }

    #[test]
    fn empty_view_links_its_root_by_semantic_key() {
        let view = DesiredClientView::empty();
        let root = view.channels.get(&view.root_channel);

        match root {
            Some(root) => {
                assert_eq!(root.key, view.root_channel);
                assert_eq!(root.parent, view.root_channel);
            }
            None => panic!("empty semantic view has no root"),
        }
    }
}
