//! The seam between the runtime and the flavor it was composed with.
//!
//! The server is compiled once and knows no business model: it holds a
//! [`FlavorRuntime`] object, and a composition binary decides which concrete
//! flavor that is. Everything business-shaped, realms included, lives behind
//! this trait and never appears in this crate.
//!
//! REF: docs/voxloom-roadmap-agents-v0_1.md P7 T8
//! REF: docs/voxloom-specification-technique-v0.1.md 24.2

use thiserror::Error;
use voxloom_control::{
    FlavorOutputValidationError, PendingPublication, PublicationCoordinator, PublicationError,
    render_snapshot, validate_rendered_snapshot,
};
use voxloom_flavor::{ConnectionId, FlavorError, SnapshotSource, VoiceEvent, VoiceFlavor};

/// Why a generation could not be produced.
///
/// The three stages are kept apart because they mean different things to an
/// operator: the flavor refused, the flavor's output was invalid, or the
/// runtime could not turn a valid output into a transition.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GenerationError {
    #[error("the flavor refused to render: {0}")]
    Render(#[from] FlavorError),
    #[error("the flavor produced an invalid generation: {0}")]
    Validation(#[from] FlavorOutputValidationError),
    #[error("the generation could not be published: {0}")]
    Publication(#[from] PublicationError),
}

/// A compiled flavor, as the runtime uses it.
///
/// Object-safe on purpose: the snapshot type is the flavor's business and never
/// crosses this boundary, so the whole render-validate-publish sequence happens
/// on the flavor's side of it.
pub trait FlavorRuntime: Send + Sync + 'static {
    /// Render the flavor's current snapshot for every registered connection,
    /// validate it, and plan the publication.
    fn plan(
        &self,
        coordinator: &mut PublicationCoordinator,
    ) -> Result<PendingPublication, GenerationError>;

    /// Report one voice event. The flavor decides alone what it changes.
    fn report(&self, event: &VoiceEvent);
}

impl<F> FlavorRuntime for F
where
    F: SnapshotSource,
{
    fn plan(
        &self,
        coordinator: &mut PublicationCoordinator,
    ) -> Result<PendingPublication, GenerationError> {
        // The connection set comes from the coordinator, not from the caller:
        // it is the only authority on who is registered, and a generation that
        // covers a different set is refused rather than published.
        let connections: Vec<ConnectionId> = coordinator.connections().collect();
        let rendered = render_snapshot(self, self.snapshot(), connections)?;
        let validated = validate_rendered_snapshot(rendered)?;
        Ok(coordinator.publish(&validated)?)
    }

    fn report(&self, event: &VoiceEvent) {
        VoiceFlavor::observe(self, event);
    }
}
