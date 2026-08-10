//! Where an arriving connection goes, and where its identity is recorded.

use std::sync::Arc;

use mumble_server_runtime_gateway::{ConnectionIdentity, ConnectionRouter, RouteDecision};
use mumble_server_runtime_shard::ConnectionId;

use crate::directory::{Destinations, Directory, Member};

/// The credential that makes a connection staff.
///
/// A placeholder for a real token flow, kept deliberately obvious: it is typed
/// into the client's password field. What matters is the shape - the gateway
/// carries an opaque credential and the application alone decides what it buys -
/// not this particular string.
const STAFF_CREDENTIAL: &str = "overwatch";

/// Everyone starts in the lobby.
///
/// Which is the point: the router answers "where does an arrival begin", not
/// "where does a player belong". The second question is the flavor's, and its
/// answer is a migration.
pub struct ArenaRouter {
    directory: Arc<Directory>,
    destinations: Arc<Destinations>,
}

impl ArenaRouter {
    #[must_use]
    pub fn new(directory: Arc<Directory>, destinations: Arc<Destinations>) -> ArenaRouter {
        ArenaRouter {
            directory,
            destinations,
        }
    }
}

impl ConnectionRouter for ArenaRouter {
    async fn route(
        &self,
        connection: ConnectionId,
        identity: &ConnectionIdentity,
    ) -> RouteDecision {
        let name = identity.name.trim();
        if name.is_empty() {
            return RouteDecision::Reject("choose a name first".to_owned());
        }

        self.directory.record(
            connection,
            Member {
                name: name.to_owned(),
                staff: identity.credential.as_deref() == Some(STAFF_CREDENTIAL),
            },
        );

        match self.destinations.lobby() {
            Some(lobby) => RouteDecision::Attach(lobby),
            // The lobby is created before the first connection can be accepted,
            // so this is unreachable in the composed binary. Refusing rather
            // than assuming a shard id keeps it that way if the composition ever
            // changes.
            None => RouteDecision::Reject("the server is still starting".to_owned()),
        }
    }
}
