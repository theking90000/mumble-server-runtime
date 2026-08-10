//! Runtime-facing host state for Controller profiles.
//!
//! The synchronization Core owns leases and fencing. This crate owns the
//! Mumble-specific binding between a logical participant, its authentication
//! credential, and at most one live runtime connection.

use std::collections::{BTreeMap, HashMap};

use mumble_server_runtime_gateway::RuntimeHandle;
use mumble_server_runtime_shard::{
    ConnectionId, ReconcileReport, ShardHandle, ShardId, ShardLogic,
};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Binding {
    credential: String,
    connection: Option<ConnectionId>,
}

#[derive(Debug, Default)]
struct Bindings {
    participants: BTreeMap<String, Binding>,
    credentials: HashMap<String, String>,
    connections: HashMap<ConnectionId, String>,
}

impl Bindings {
    fn register(&mut self, participant_id: &str, credential: String) -> Result<(), HostError> {
        if participant_id.is_empty() || credential.is_empty() {
            return Err(HostError::InvalidRegistration);
        }
        if self
            .credentials
            .get(&credential)
            .is_some_and(|owner| owner != participant_id)
        {
            return Err(HostError::CredentialConflict);
        }

        let connection = self
            .participants
            .remove(participant_id)
            .and_then(|previous| {
                self.credentials.remove(&previous.credential);
                previous.connection
            });
        self.credentials
            .insert(credential.clone(), participant_id.to_owned());
        self.participants.insert(
            participant_id.to_owned(),
            Binding {
                credential,
                connection,
            },
        );
        Ok(())
    }

    fn attach(
        &mut self,
        credential: &str,
        connection: ConnectionId,
    ) -> Result<Attachment, HostError> {
        let participant_id = self
            .credentials
            .get(credential)
            .cloned()
            .ok_or(HostError::UnknownCredential)?;
        if self
            .connections
            .get(&connection)
            .is_some_and(|owner| owner != &participant_id)
        {
            return Err(HostError::ConnectionConflict);
        }
        let binding = self
            .participants
            .get_mut(&participant_id)
            .ok_or(HostError::InconsistentBinding)?;
        let replaced_connection = binding.connection.replace(connection);
        if let Some(previous) = replaced_connection {
            self.connections.remove(&previous);
        }
        self.connections.insert(connection, participant_id.clone());
        Ok(Attachment {
            participant_id,
            replaced_connection: replaced_connection.filter(|previous| *previous != connection),
        })
    }

    fn disconnect(&mut self, connection: ConnectionId) -> Option<String> {
        let participant_id = self.connections.remove(&connection)?;
        if let Some(binding) = self.participants.get_mut(&participant_id)
            && binding.connection == Some(connection)
        {
            binding.connection = None;
        }
        Some(participant_id)
    }

    fn remove(&mut self, participant_id: &str) -> Option<ConnectionId> {
        let binding = self.participants.remove(participant_id)?;
        self.credentials.remove(&binding.credential);
        if let Some(connection) = binding.connection {
            self.connections.remove(&connection);
        }
        binding.connection
    }

    fn credential(&self, participant_id: &str) -> Option<&str> {
        self.participants
            .get(participant_id)
            .map(|binding| binding.credential.as_str())
    }

    fn connection(&self, participant_id: &str) -> Option<ConnectionId> {
        self.participants
            .get(participant_id)
            .and_then(|binding| binding.connection)
    }

    fn participant(&self, connection: ConnectionId) -> Option<&str> {
        self.connections.get(&connection).map(String::as_str)
    }

    fn contains(&self, participant_id: &str) -> bool {
        self.participants.contains_key(participant_id)
    }
}

/// A successful authentication and connection binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub participant_id: String,
    pub replaced_connection: Option<ConnectionId>,
}

/// A host-side authentication or binding failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HostError {
    #[error("participant registrations and credentials must not be empty")]
    InvalidRegistration,
    #[error("the Mumble credential is already assigned")]
    CredentialConflict,
    #[error("the Mumble credential is unknown or revoked")]
    UnknownCredential,
    #[error("the Mumble connection is already assigned")]
    ConnectionConflict,
    #[error("the host binding indexes disagree")]
    InconsistentBinding,
}

/// Mumble-specific state and runtime operations used by the Controller server.
#[derive(Debug)]
pub struct MumbleHost {
    runtime: RuntimeHandle,
    bindings: Bindings,
}

impl MumbleHost {
    #[must_use]
    pub fn new(runtime: RuntimeHandle) -> Self {
        Self {
            runtime,
            bindings: Bindings::default(),
        }
    }

    /// Install or rotate a participant credential while retaining its connection.
    pub fn register(&mut self, participant_id: &str, credential: String) -> Result<(), HostError> {
        self.bindings.register(participant_id, credential)
    }

    /// Authenticate a connection and replace any older connection for the participant.
    pub fn attach(
        &mut self,
        credential: &str,
        connection: ConnectionId,
    ) -> Result<Attachment, HostError> {
        let attachment = self.bindings.attach(credential, connection)?;
        if let Some(previous) = attachment.replaced_connection {
            self.close_connection(previous);
        }
        Ok(attachment)
    }

    /// Forget a runtime-reported departure if it still names the active connection.
    pub fn disconnect(&mut self, connection: ConnectionId) -> Option<String> {
        self.bindings.disconnect(connection)
    }

    /// Revoke authentication and close the participant's current connection.
    pub fn revoke(&mut self, participant_id: &str) {
        if let Some(connection) = self.bindings.remove(participant_id) {
            self.close_connection(connection);
        }
    }

    #[must_use]
    pub fn contains(&self, participant_id: &str) -> bool {
        self.bindings.contains(participant_id)
    }

    /// Whether a freshly generated credential can be installed without aliasing.
    #[must_use]
    pub fn credential_available(&self, credential: &str) -> bool {
        !credential.is_empty() && !self.bindings.credentials.contains_key(credential)
    }

    #[must_use]
    pub fn credential(&self, participant_id: &str) -> Option<&str> {
        self.bindings.credential(participant_id)
    }

    #[must_use]
    pub fn connection(&self, participant_id: &str) -> Option<ConnectionId> {
        self.bindings.connection(participant_id)
    }

    #[must_use]
    pub fn participant(&self, connection: ConnectionId) -> Option<&str> {
        self.bindings.participant(connection)
    }

    pub fn move_connection(&self, connection: ConnectionId, to: ShardId) {
        self.runtime.move_connection(connection, to);
    }

    pub fn create_shard_with_reports<L, O>(
        &self,
        build: impl FnOnce(ShardHandle) -> L,
        observer: O,
    ) -> ShardHandle
    where
        L: ShardLogic,
        O: FnMut(&ReconcileReport) + Send + 'static,
    {
        self.runtime.create_shard_with_reports(build, observer)
    }

    pub fn destroy_shard(&self, shard: ShardId, reason: &str) {
        self.runtime.destroy_shard(shard, reason);
    }

    fn close_connection(&self, connection: ConnectionId) {
        if let Some(peer) = self.runtime.peers().by_connection(connection) {
            peer.close();
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn credential_rotation_keeps_the_live_connection_and_revokes_the_old_token() {
        let mut bindings = Bindings::default();
        bindings
            .register("alice", "first".to_owned())
            .expect("register first credential");
        bindings
            .attach("first", ConnectionId(7))
            .expect("attach first connection");

        bindings
            .register("alice", "second".to_owned())
            .expect("rotate credential");

        assert_eq!(bindings.connection("alice"), Some(ConnectionId(7)));
        assert_eq!(
            bindings.attach("first", ConnectionId(8)),
            Err(HostError::UnknownCredential)
        );
        assert_eq!(bindings.credential("alice"), Some("second"));
    }

    #[test]
    fn replacement_and_departure_keep_all_indexes_consistent() {
        let mut bindings = Bindings::default();
        bindings
            .register("alice", "join".to_owned())
            .expect("register credential");
        bindings
            .attach("join", ConnectionId(7))
            .expect("attach first connection");
        let replacement = bindings
            .attach("join", ConnectionId(8))
            .expect("replace connection");

        assert_eq!(replacement.replaced_connection, Some(ConnectionId(7)));
        assert_eq!(bindings.participant(ConnectionId(7)), None);
        assert_eq!(bindings.participant(ConnectionId(8)), Some("alice"));
        assert_eq!(
            bindings.disconnect(ConnectionId(8)),
            Some("alice".to_owned())
        );
        assert_eq!(bindings.connection("alice"), None);
    }

    #[test]
    fn credentials_and_connections_are_unique_across_participants() {
        let mut bindings = Bindings::default();
        bindings
            .register("alice", "alice-token".to_owned())
            .expect("register Alice");
        bindings
            .register("bob", "bob-token".to_owned())
            .expect("register Bob");
        bindings
            .attach("alice-token", ConnectionId(7))
            .expect("attach Alice");

        assert_eq!(
            bindings.register("bob", "alice-token".to_owned()),
            Err(HostError::CredentialConflict)
        );
        assert_eq!(
            bindings.attach("bob-token", ConnectionId(7)),
            Err(HostError::ConnectionConflict)
        );
    }
}
