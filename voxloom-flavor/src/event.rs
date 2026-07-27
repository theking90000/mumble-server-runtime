//! Voice-plane facts reported to the flavor.
//!
//! Every event is a statement about the voice runtime, never a business
//! command: Voxloom reports that a client asked for something, and the flavor
//! alone decides what its state becomes. A flavor that refuses simply publishes
//! nothing new, and the connection keeps the view it already holds.
//!
//! REF: docs/voxloom-specification-technique-v0.1.md 24.1, 24.4

use crate::{ChannelKey, ConnectionId};

/// One voice event, stamped with the generation it was resolved against.
///
/// The stamp is what makes an interaction verifiable: it was resolved in the
/// view Voxloom had committed for that generation, so a flavor revalidating it
/// against a newer snapshot can tell that the client acted on stale
/// information (spec 24.4).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum VoiceEvent {
    /// A voice connection became live and holds a committed view.
    ///
    /// The name and certificate hash are the authenticated presentation
    /// identity the runtime obtained during the handshake (spec 10). They are
    /// the flavor's only input for deciding who this connection is; the runtime
    /// itself derives no business meaning from them.
    Connected {
        connection: ConnectionId,
        generation: u64,
        name: String,
        certificate_hash: Option<String>,
    },
    /// A connection was refused before it ever held a view.
    AuthenticationFailed {
        connection: ConnectionId,
        generation: u64,
        reason: String,
    },
    /// A client asked to act on a channel of its own view, expressed by the
    /// semantic key the flavor declared. What the request means, and whether it
    /// is granted, belongs entirely to the flavor.
    ChannelInteractionRequested {
        connection: ConnectionId,
        generation: u64,
        channel: ChannelKey,
    },
    /// A voice connection is gone. No further event will carry it.
    Disconnected {
        connection: ConnectionId,
        generation: u64,
        reason: String,
    },
}

impl VoiceEvent {
    /// The connection this event is about.
    #[must_use]
    pub const fn connection(&self) -> ConnectionId {
        match self {
            Self::Connected { connection, .. }
            | Self::AuthenticationFailed { connection, .. }
            | Self::ChannelInteractionRequested { connection, .. }
            | Self::Disconnected { connection, .. } => *connection,
        }
    }

    /// The Voxloom generation whose committed view this event was resolved in.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        match self {
            Self::Connected { generation, .. }
            | Self::AuthenticationFailed { generation, .. }
            | Self::ChannelInteractionRequested { generation, .. }
            | Self::Disconnected { generation, .. } => *generation,
        }
    }
}
