//! Who is who, and where the shards are.
//!
//! The runtime deliberately tells a flavor nothing but a [`ConnectionId`]: it
//! has no opinion on what a user is, and inventing one would put business in the
//! runtime. So the application keeps its own directory, filled by the router at
//! the one moment identity and identifier meet, and read by both shards.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use voxloom_shard::{ConnectionId, ShardId, UserFlags};

/// What this application knows about one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The display name, already bounded by the gateway.
    pub name: String,
    /// Whether this connection presented the staff credential.
    ///
    /// A stand-in for a real token flow, and the honest shape of one: the
    /// gateway hands the credential over without interpreting it, and the
    /// application decides what it is worth.
    pub staff: bool,
}

/// Connections, by identifier.
#[derive(Debug, Default)]
pub struct Directory {
    members: Mutex<BTreeMap<ConnectionId, Member>>,
    /// What each connection asked for its own audio state.
    ///
    /// Here rather than in a shard because a migration moves a connection
    /// between two of them: a player who muted itself in the lobby stays muted
    /// when the arena takes it. Kept apart from [`Member`], which is identity
    /// the router writes once, while this changes at every click.
    voice: Mutex<BTreeMap<ConnectionId, UserFlags>>,
}

impl Directory {
    #[must_use]
    pub fn new() -> Directory {
        Directory::default()
    }

    pub fn record(&self, connection: ConnectionId, member: Member) {
        guard(&self.members).insert(connection, member);
    }

    /// Drop a connection. Called by whichever shard sees it leave, so an entry
    /// never outlives the connection it describes.
    pub fn forget(&self, connection: ConnectionId) {
        guard(&self.members).remove(&connection);
        guard(&self.voice).remove(&connection);
    }

    /// Grant a self-mute or self-deafen request.
    ///
    /// This application says yes to both, which is the ordinary policy: nothing
    /// in an arena depends on hearing someone who asked not to speak. A flag the
    /// client did not mention keeps whatever is already rendered, so a message
    /// about one flag never silently clears the other.
    pub fn set_self_state(
        &self,
        connection: ConnectionId,
        self_mute: Option<bool>,
        self_deaf: Option<bool>,
    ) {
        let mut voice = guard(&self.voice);
        let entry = voice.entry(connection).or_default();
        if let Some(mute) = self_mute {
            entry.self_mute = mute;
        }
        if let Some(deaf) = self_deaf {
            entry.self_deaf = deaf;
        }
    }

    /// The flags to render for a connection. Unknown means nothing was asked.
    #[must_use]
    pub fn flags(&self, connection: ConnectionId) -> UserFlags {
        guard(&self.voice)
            .get(&connection)
            .copied()
            .unwrap_or_default()
    }

    #[must_use]
    pub fn member(&self, connection: ConnectionId) -> Option<Member> {
        guard(&self.members).get(&connection).cloned()
    }

    /// The name to render, or a placeholder.
    ///
    /// A connection with no entry is possible in exactly one case: it arrived
    /// through a router that did not record it. Rendering it under a visible
    /// placeholder is better than hiding it, because a user nobody can see is a
    /// user nobody can be told to stop talking to.
    #[must_use]
    pub fn name(&self, connection: ConnectionId) -> String {
        self.member(connection)
            .map_or_else(|| format!("unknown-{}", connection.0), |member| member.name)
    }

    #[must_use]
    pub fn is_staff(&self, connection: ConnectionId) -> bool {
        self.member(connection).is_some_and(|member| member.staff)
    }
}

/// The shards this application runs, published once each.
///
/// Two flavors that can send connections to each other have to learn the other's
/// identifier, and neither exists when the first is built. A write-once cell is
/// the smallest thing that resolves it without a placeholder that could be read
/// before it is filled.
#[derive(Debug, Default)]
pub struct Destinations {
    lobby: OnceLock<ShardId>,
    arena: OnceLock<ShardId>,
}

impl Destinations {
    #[must_use]
    pub fn new() -> Destinations {
        Destinations::default()
    }

    pub fn set_lobby(&self, shard: ShardId) {
        let _first = self.lobby.set(shard);
    }

    pub fn set_arena(&self, shard: ShardId) {
        let _first = self.arena.set(shard);
    }

    #[must_use]
    pub fn lobby(&self) -> Option<ShardId> {
        self.lobby.get().copied()
    }

    #[must_use]
    pub fn arena(&self) -> Option<ShardId> {
        self.arena.get().copied()
    }
}

/// A poisoned directory means a panic unwound mid-update. The map is still
/// sound, and taking the whole application down over one thread would be a worse
/// outcome than a stale name.
fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
