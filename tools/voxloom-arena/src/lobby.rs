//! The lobby: one flat room where everyone sees and hears everyone.
//!
//! Deliberately the simplest shard in the application. Scopes buy nothing here -
//! there is one group - and using them anyway would be the mistake of reaching
//! for a mechanism because it exists.
//!
//! Its whole job is to let a player state an intent by double-clicking a
//! channel, and to hand them to the arena when they ask for it. A small external
//! counter also demonstrates how business work reaches a shard: the producer
//! sends an update, wakes the shard, and `render` drains the flavor-owned inbox.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::mpsc;
use voxloom_shard::{
    ActionKey, Audience, ChannelKey, ConnectionId, DomainId, Narrow, Occupant, On, Reply, Scope,
    ScopeSet, ShardBuilder, ShardLogic, VoiceEvent,
};

use crate::arena::Side;
use crate::directory::{Destinations, Directory};

const RED: ChannelKey = ChannelKey(1);
const BLUE: ChannelKey = ChannelKey(2);
const SPECTATE: ChannelKey = ChannelKey(3);
const ENTER: ChannelKey = ChannelKey(4);

/// The same door as the `ENTER` channel, as a button.
///
/// Offered per connection rather than shared, because that is what a context
/// action is: a private declaration. A player who has not chosen a side is told
/// so when they press it, which a channel cannot do.
const JOIN: ActionKey = ActionKey(1);

/// Everyone in the lobby hears everyone else.
const LOBBY_VOICE: DomainId = DomainId(1);

/// What a connection has asked for while waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Undecided,
    Join(Side),
    Spectate,
}

impl Intent {
    /// The role this intent becomes once the arena takes the player.
    #[must_use]
    pub fn as_side(self) -> Option<Side> {
        match self {
            Intent::Join(side) => Some(side),
            Intent::Undecided | Intent::Spectate => None,
        }
    }
}

/// A business update produced outside the shard runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LobbyUpdate {
    Counter(u64),
}

/// The lobby shard's logic.
pub struct Lobby {
    directory: Arc<Directory>,
    destinations: Arc<Destinations>,
    waiting: BTreeMap<ConnectionId, Intent>,
    /// What each connection asked for, read by the arena when it takes them.
    chosen: Arc<Choices>,
    updates: mpsc::Receiver<LobbyUpdate>,
    counter: u64,
}

/// The intent a player carried into the arena.
///
/// Shared rather than sent: a migration moves a connection, not a message, and
/// the two shards need one place to agree on what the player asked for.
#[derive(Debug, Default)]
pub struct Choices {
    intents: std::sync::Mutex<BTreeMap<ConnectionId, Intent>>,
}

impl Choices {
    #[must_use]
    pub fn new() -> Choices {
        Choices::default()
    }

    pub fn set(&self, connection: ConnectionId, intent: Intent) {
        self.guard().insert(connection, intent);
    }

    #[must_use]
    pub fn get(&self, connection: ConnectionId) -> Intent {
        self.guard()
            .get(&connection)
            .copied()
            .unwrap_or(Intent::Undecided)
    }

    pub fn forget(&self, connection: ConnectionId) {
        self.guard().remove(&connection);
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, BTreeMap<ConnectionId, Intent>> {
        self.intents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Lobby {
    #[must_use]
    pub fn new(
        directory: Arc<Directory>,
        destinations: Arc<Destinations>,
        chosen: Arc<Choices>,
        updates: mpsc::Receiver<LobbyUpdate>,
    ) -> Lobby {
        Lobby {
            directory,
            destinations,
            waiting: BTreeMap::new(),
            chosen,
            updates,
            counter: 0,
        }
    }

    /// What a connection is currently asking for. Exposed for tests.
    #[must_use]
    pub fn intent(&self, connection: ConnectionId) -> Intent {
        self.waiting
            .get(&connection)
            .copied()
            .unwrap_or(Intent::Undecided)
    }

    #[must_use]
    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }
}

impl ShardLogic for Lobby {
    fn render(&mut self, out: &mut ShardBuilder<'_>) {
        while let Ok(update) = self.updates.try_recv() {
            match update {
                LobbyUpdate::Counter(counter) => self.counter = counter,
            }
        }

        let root_name = format!("Mumble Server Runtime Arena | update {}", self.counter);
        let root = out.root(&root_name);
        // Every channel stays at the root scope: one group, one view, and the
        // whole lobby is a single delta for everyone.
        let red = out.channel(root, RED, "Red Team", Narrow::Same);
        let blue = out.channel(root, BLUE, "Blue Team", Narrow::Same);
        let spectate = out.channel(root, SPECTATE, "Spectators", Narrow::Same);
        let enter = out.channel(root, ENTER, "> Enter the Arena", Narrow::Same);
        out.channel_position(red, 1);
        out.channel_position(blue, 2);
        out.channel_position(spectate, 3);
        out.channel_position(enter, 4);
        out.channel_can_text(enter, false);

        for connection in self.waiting.keys().copied() {
            out.private(connection, |private| {
                private.action(JOIN, "Enter the Arena", On::SERVER);
            });
        }

        for (connection, intent) in &self.waiting {
            let placement = match intent {
                Intent::Undecided => root,
                Intent::Join(Side::Red) => red,
                Intent::Join(Side::Blue) => blue,
                Intent::Spectate => spectate,
            };
            let name = self.directory.name(*connection);
            let user = out.user(
                placement,
                Occupant::Connection(*connection),
                &name,
                Narrow::Same,
            );
            out.user_flags(user, self.directory.flags(*connection));
        }

        let everyone: Vec<ConnectionId> = self.waiting.keys().copied().collect();
        out.audio_domain(LOBBY_VOICE, &everyone);
    }

    fn observation(&mut self, _connection: ConnectionId) -> ScopeSet {
        // One group means one observation, and it is the same one for everyone.
        ScopeSet::new(&[Scope::ROOT]).unwrap_or(ScopeSet::NONE)
    }

    fn observe(&mut self, event: &VoiceEvent, out: &mut Reply) {
        match event {
            VoiceEvent::Connected { connection } => {
                let intent = self.chosen.get(*connection);
                self.waiting.insert(*connection, intent);
            }
            VoiceEvent::Disconnected { connection, .. } => {
                self.waiting.remove(connection);
                self.chosen.forget(*connection);
                self.directory.forget(*connection);
            }
            VoiceEvent::Migrated { connection, .. } => {
                // It is going to the arena, not leaving: its directory entry and
                // its choice both have to survive the move.
                self.waiting.remove(connection);
            }
            VoiceEvent::RequestedChannel {
                connection,
                channel,
            } => self.requested(*connection, *channel, out),
            // The button and the channel are two spellings of one intent, so
            // they land in the same place. Anything else this build ever offers
            // gets its own arm rather than a shared default.
            VoiceEvent::InvokedAction {
                connection, action, ..
            } if *action == JOIN => self.requested(*connection, ENTER, out),
            // Granted, and stored where a migration will find it again.
            VoiceEvent::RequestedSelfState {
                connection,
                self_mute,
                self_deaf,
            } => self
                .directory
                .set_self_state(*connection, *self_mute, *self_deaf),
            // The lobby is one group at one scope, so everybody can see
            // everybody: there is no audience to second-guess, and carrying the
            // message out is one line.
            VoiceEvent::Said {
                connection,
                to,
                text,
            } => out.relay(*connection, *to, text),
            // The event enum is non-exhaustive on purpose: a runtime that starts
            // reporting something new must not silently change what this flavor
            // does.
            other => eprintln!("voxloom-arena: the lobby ignores {other:?}"),
        }
    }
}

impl Lobby {
    fn requested(&mut self, connection: ConnectionId, channel: ChannelKey, out: &mut Reply) {
        let intent = match channel {
            RED => Intent::Join(Side::Red),
            BLUE => Intent::Join(Side::Blue),
            SPECTATE => Intent::Spectate,
            ENTER => {
                let intent = self.intent(connection);
                self.chosen.set(connection, intent);
                match self.destinations.arena() {
                    Some(arena) => {
                        out.say(connection, "Entering the arena.");
                        // Said by the server rather than relayed from the player,
                        // which is what lets it reach the whole lobby including
                        // the person leaving: nobody is skipped for not seeing an
                        // actor there is none of.
                        out.announce(
                            Audience::Tree(ChannelKey::ROOT),
                            &format!("{} has entered the arena.", self.directory.name(connection)),
                        );
                        out.switch(connection, arena);
                    }
                    // Refusing out loud rather than only in the server's log: the
                    // player double-clicked and is owed an answer, and without one
                    // the door simply looks broken.
                    None => out.refuse(connection, "The arena is not running yet."),
                }
                return;
            }
            // The root, or a channel this build does not act on.
            _ => Intent::Undecided,
        };
        self.waiting.insert(connection, intent);
        self.chosen.set(connection, intent);
    }
}
