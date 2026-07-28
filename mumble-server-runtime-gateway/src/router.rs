//! Where a connection goes when it has just authenticated.
//!
//! A connection that has just proven who it is belongs to no shard yet, and the
//! decision cannot come from a shard: none of them knows about it, and asking
//! them all would be a broadcast for every arrival. So it is a runtime-level
//! policy, and this is its whole surface.
//!
//! It runs **on the connection's own task**, never on a shard's, which is what
//! makes it free to await: validating a token, calling out to a service, or
//! reading a database blocks one arrival rather than a whole shard.
//!
//! REF: docs/design/guide-implementation.md 10.1

use mumble_server_runtime_shard::{ConnectionId, ShardId};

/// What the gateway knows about a connection at the moment it must be routed.
///
/// Everything here comes from the client, so all of it is a claim. The one piece
/// that carries cryptographic weight is [`ConnectionIdentity::certificate_hash`]:
/// TLS proved possession of the matching private key. It still proves identity
/// and not authority - what a certificate is *allowed* to do is the router's
/// business, not the gateway's.
#[derive(Debug, Clone)]
pub struct ConnectionIdentity {
    /// The name the client proposed. A suggestion: the server is authoritative
    /// on what a user is finally called (spec 10.5).
    pub name: String,
    /// Lowercase SHA-1 of the client certificate, as Murmur presents it.
    pub certificate_hash: Option<String>,
    /// The `Authenticate.password` field, treated as an opaque credential. This
    /// is where a token flow plugs in.
    pub credential: Option<String>,
}

/// The routing verdict.
#[derive(Debug, Clone)]
pub enum RouteDecision {
    /// Attach to this shard. The shard must exist; if it has been destroyed in
    /// the meantime the connection is refused rather than stranded.
    Attach(ShardId),
    /// Refuse, with a reason the client is shown.
    Reject(String),
}

/// The policy that answers "which shard?".
///
/// Generic rather than `dyn` on purpose: an `async fn` in a trait is not
/// dyn-compatible, and boxing every routing decision to work around that would
/// buy nothing. A deployment that genuinely needs runtime polymorphism writes
/// one router that dispatches internally.
pub trait ConnectionRouter: Send + Sync + 'static {
    /// Decide where this connection belongs.
    ///
    /// Runs on the connection's task. It may await, and it may take its time:
    /// the cost is borne by the arriving client alone.
    ///
    /// The identifier is handed over as well as the claim, because this is the
    /// only moment where the two meet. A shard's [`mumble_server_runtime_shard::VoiceEvent`]
    /// carries a `ConnectionId` and nothing else - deliberately, since the
    /// runtime has no opinion on what a user *is* - so an application that wants
    /// its flavor to know a name records the pair here.
    fn route(
        &self,
        connection: ConnectionId,
        identity: &ConnectionIdentity,
    ) -> impl std::future::Future<Output = RouteDecision> + Send;
}

/// A router that sends everyone to the same shard.
///
/// The honest default for a runtime with one entry point, and what a lobby
/// looks like: the *flavor* decides where anyone goes next, which is a
/// migration rather than a routing decision.
#[derive(Debug, Clone, Copy)]
pub struct AlwaysAttach(pub ShardId);

impl ConnectionRouter for AlwaysAttach {
    async fn route(
        &self,
        _connection: ConnectionId,
        _identity: &ConnectionIdentity,
    ) -> RouteDecision {
        RouteDecision::Attach(self.0)
    }
}
