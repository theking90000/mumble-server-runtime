//! The arena: two teams that cannot see each other, spectators who hear both,
//! and an admin nobody sees until they choose to be heard.
//!
//! This is where the three mechanisms are used for three different things.
//!
//! # Teams are scopes
//!
//! A red player observes the red scope. Blue Base is not hidden from them by a
//! filter they could be given the wrong side of: it is at a scope their
//! observation is not comparable to, so it is absent from every delta they will
//! ever receive. The same render produces both teams' views, once.
//!
//! # The admin is an overlay
//!
//! Staff are in no shared channel. They exist only in overlays: their own, which
//! is what lets the client find itself at `ServerSync`, and - when they address
//! a team - that team's.
//!
//! # Hearing is a relation, not a place
//!
//! Spectators sit at a scope of their own and `listen` to both teams: one-way by
//! construction, not by a rule someone has to remember. The admin listens to
//! everything and is heard by nobody, until an explicit edge is opened.
//!
//! An edge and an overlay entry always go together here, and that is not a
//! convention: the render is refused if a receiver cannot see its sender,
//! because the Mumble client discards audio from a session it does not know.

use std::collections::BTreeMap;
use std::sync::Arc;

use voxloom_shard::{
    Audience, ChannelKey, ConnectionId, DomainId, Narrow, Occupant, Reply, Scope, ScopeSet,
    ShardBuilder, ShardLogic, VoiceEvent,
};

use crate::directory::{Destinations, Directory};
use crate::lobby::{Choices, Intent};

const RED_BASE: ChannelKey = ChannelKey(10);
const BLUE_BASE: ChannelKey = ChannelKey(11);
const DECK: ChannelKey = ChannelKey(12);
const NEUTRAL: ChannelKey = ChannelKey(13);
const BACK: ChannelKey = ChannelKey(14);

/// Private channel keys start well past the shared ones and are offset by the
/// connection, so two admins never share one.
const OVERWATCH_BASE: u64 = 1 << 32;

const RED_VOICE: DomainId = DomainId(1);
const BLUE_VOICE: DomainId = DomainId(2);
const DECK_VOICE: DomainId = DomainId(3);

/// Which team.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Side {
    Red,
    Blue,
}

impl Side {
    /// The segment this side narrows the root scope by.
    const fn segment(self) -> u32 {
        match self {
            Side::Red => 1,
            Side::Blue => 2,
        }
    }

    /// The base this side occupies.
    const fn base(self) -> ChannelKey {
        match self {
            Side::Red => RED_BASE,
            Side::Blue => BLUE_BASE,
        }
    }
}

/// The scope segment spectators and staff observe.
const DECK_SEGMENT: u32 = 3;

/// What a connection is in the arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// On a team. Sees its own team and nothing of the other.
    Player(Side),
    /// Sees everything, is heard by nobody.
    Spectator,
    /// In no shared channel at all.
    Admin {
        /// The team currently being addressed, if any. Setting it is what makes
        /// the admin both visible and audible, to exactly that team.
        addressing: Option<Side>,
    },
}

/// The arena shard's logic.
pub struct Arena {
    directory: Arc<Directory>,
    destinations: Arc<Destinations>,
    chosen: Arc<Choices>,
    roles: BTreeMap<ConnectionId, Role>,
}

impl Arena {
    #[must_use]
    pub fn new(
        directory: Arc<Directory>,
        destinations: Arc<Destinations>,
        chosen: Arc<Choices>,
    ) -> Arena {
        Arena {
            directory,
            destinations,
            chosen,
            roles: BTreeMap::new(),
        }
    }

    /// What a connection currently is. Exposed for tests.
    #[must_use]
    pub fn role(&self, connection: ConnectionId) -> Option<Role> {
        self.roles.get(&connection).copied()
    }

    #[must_use]
    pub fn population(&self) -> usize {
        self.roles.len()
    }

    fn members_of(&self, side: Side) -> Vec<ConnectionId> {
        self.roles
            .iter()
            .filter(|(_, role)| **role == Role::Player(side))
            .map(|(connection, _)| *connection)
            .collect()
    }

    fn spectators(&self) -> Vec<ConnectionId> {
        self.roles
            .iter()
            .filter(|(_, role)| **role == Role::Spectator)
            .map(|(connection, _)| *connection)
            .collect()
    }

    fn admins(&self) -> Vec<(ConnectionId, Option<Side>)> {
        self.roles
            .iter()
            .filter_map(|(connection, role)| match role {
                Role::Admin { addressing } => Some((*connection, *addressing)),
                _ => None,
            })
            .collect()
    }
}

/// A team's scope: one step below the root.
fn side_scope(side: Side) -> Scope {
    Scope::ROOT.child(side.segment()).unwrap_or(Scope::ROOT)
}

fn deck_scope() -> Scope {
    Scope::ROOT.child(DECK_SEGMENT).unwrap_or(Scope::ROOT)
}

/// What someone who is allowed to watch the whole arena observes.
fn watcher_view() -> ScopeSet {
    ScopeSet::new(&[side_scope(Side::Red), side_scope(Side::Blue), deck_scope()])
        .unwrap_or(ScopeSet::NONE)
}

impl ShardLogic for Arena {
    fn render(&mut self, out: &mut ShardBuilder<'_>) {
        let root = out.root("The Arena");
        let red = out.channel(
            root,
            RED_BASE,
            "Red Base",
            Narrow::Into(Side::Red.segment()),
        );
        let blue = out.channel(
            root,
            BLUE_BASE,
            "Blue Base",
            Narrow::Into(Side::Blue.segment()),
        );
        let deck = out.channel(root, DECK, "Observation Deck", Narrow::Into(DECK_SEGMENT));
        // Both of these stay at the root scope, which is what makes them the
        // only two things everyone in the arena can act on.
        let neutral = out.channel(root, NEUTRAL, "Neutral Ground", Narrow::Same);
        let back = out.channel(root, BACK, "< Back to the Lobby", Narrow::Same);

        out.channel_position(red, 1);
        out.channel_position(blue, 2);
        out.channel_position(deck, 3);
        out.channel_position(neutral, 4);
        out.channel_position(back, 5);

        // A door is not a room: the client greys its chat box out here rather
        // than letting someone type into something nobody is standing in.
        out.channel_can_text(back, false);

        for (connection, role) in &self.roles {
            let name = self.directory.name(*connection);
            let flags = self.directory.flags(*connection);
            match role {
                Role::Player(Side::Red) => {
                    let user =
                        out.user(red, Occupant::Connection(*connection), &name, Narrow::Same);
                    out.user_flags(user, flags);
                }
                Role::Player(Side::Blue) => {
                    let user =
                        out.user(blue, Occupant::Connection(*connection), &name, Narrow::Same);
                    out.user_flags(user, flags);
                }
                Role::Spectator => {
                    let user =
                        out.user(deck, Occupant::Connection(*connection), &name, Narrow::Same);
                    out.user_flags(user, flags);
                }
                // Staff have no shared presence. This is the vanish, and it is
                // an absence rather than a flag: there is nothing to filter out
                // downstream, because nothing was ever written.
                Role::Admin { .. } => {}
            }
        }

        let red_members = self.members_of(Side::Red);
        let blue_members = self.members_of(Side::Blue);
        let spectators = self.spectators();

        // A team hears itself.
        out.audio_domain(RED_VOICE, &red_members);
        out.audio_domain(BLUE_VOICE, &blue_members);
        out.audio_domain(DECK_VOICE, &spectators);

        // Spectators hear both teams and are heard by neither. Being one-way is
        // what `listen` *is*, so there is no rule to get wrong.
        for spectator in &spectators {
            out.audio_listen(*spectator, RED_VOICE);
            out.audio_listen(*spectator, BLUE_VOICE);
        }

        for (admin, addressing) in self.admins() {
            let name = self.directory.name(admin);
            let flags = self.directory.flags(admin);

            // The admin's own presence, visible to nobody else. Without it the
            // client has no self to find when `ServerSync` names its session.
            let overwatch = ChannelKey(OVERWATCH_BASE.wrapping_add(admin.0));
            out.private(admin, |private| {
                let room = private.channel(root, overwatch, "Overwatch");
                let user = private.user_in(
                    room,
                    Occupant::Connection(admin),
                    &format!("{name} (vanished)"),
                );
                private.user_flags(user, flags);
            });

            // Staff hear everything, and are heard by nobody.
            out.audio_listen(admin, RED_VOICE);
            out.audio_listen(admin, BLUE_VOICE);
            out.audio_listen(admin, DECK_VOICE);

            let Some(side) = addressing else {
                continue;
            };
            let base = match side {
                Side::Red => red,
                Side::Blue => blue,
            };
            // Being heard and being seen are one decision. The edge alone would
            // build a render the shard refuses, because a receiver that cannot
            // see its sender is a receiver whose client throws the audio away.
            for member in self.members_of(side) {
                out.private(member, |private| {
                    let user = private.user_in(
                        base,
                        Occupant::Connection(admin),
                        &format!("[Staff] {name}"),
                    );
                    private.user_flags(user, flags);
                });
                out.audio_edge(admin, member);
            }
        }
    }

    fn observation(&mut self, connection: ConnectionId) -> ScopeSet {
        match self.roles.get(&connection) {
            Some(Role::Player(side)) => {
                ScopeSet::new(&[side_scope(*side)]).unwrap_or(ScopeSet::NONE)
            }
            Some(Role::Spectator | Role::Admin { .. }) => watcher_view(),
            // Attached but not yet placed: it sees the root and its two
            // root-scoped channels, which is enough to ask for a team.
            None => ScopeSet::new(&[Scope::ROOT]).unwrap_or(ScopeSet::NONE),
        }
    }

    fn observe(&mut self, event: &VoiceEvent, out: &mut Reply) {
        match event {
            VoiceEvent::Connected { connection } => {
                let role = if self.directory.is_staff(*connection) {
                    Role::Admin { addressing: None }
                } else {
                    match self.chosen.get(*connection).as_side() {
                        Some(side) => Role::Player(side),
                        // Undecided or spectating: watching is the safe default,
                        // since it grants no voice into either team.
                        None => Role::Spectator,
                    }
                };
                self.roles.insert(*connection, role);
            }
            VoiceEvent::Disconnected { connection, .. } => {
                self.roles.remove(connection);
                self.chosen.forget(*connection);
                self.directory.forget(*connection);
            }
            VoiceEvent::Migrated { connection, .. } => {
                self.roles.remove(connection);
            }
            VoiceEvent::RequestedChannel {
                connection,
                channel,
            } => self.requested(*connection, *channel, out),
            // Granted, including for a vanished admin: the flag rides in its own
            // overlay, so muting itself changes nothing anyone else can see.
            VoiceEvent::RequestedSelfState {
                connection,
                self_mute,
                self_deaf,
            } => self
                .directory
                .set_self_state(*connection, *self_mute, *self_deaf),
            VoiceEvent::Said {
                connection,
                to,
                text,
            } => self.said(*connection, *to, text, out),
            other => eprintln!("voxloom-arena: the arena ignores {other:?}"),
        }
    }
}

impl Arena {
    /// Where a role may write, and what happens when it writes elsewhere.
    ///
    /// The runtime already drops a recipient who cannot see the sender, so a
    /// spectator typing into Red Base would reach nobody and hear nothing back.
    /// Refusing out loud is the difference between a rule and a bug: the writer
    /// learns why, instead of watching their message evaporate.
    ///
    /// A private message is left alone. Its audience is one person, the runtime
    /// checks that person can see the sender, and there is nothing a team rule
    /// could add.
    fn said(&mut self, connection: ConnectionId, to: Audience, text: &str, out: &mut Reply) {
        if matches!(to, Audience::User(_)) {
            out.relay(connection, to, text);
            return;
        }

        let Some(role) = self.roles.get(&connection).copied() else {
            return;
        };
        let heard_in = match role {
            Role::Player(side) => side.base(),
            Role::Spectator => DECK,
            // Addressing a side is what puts staff in its base, in an overlay,
            // and being visible there is exactly what a relay needs.
            Role::Admin {
                addressing: Some(side),
            } => side.base(),
            Role::Admin { addressing: None } => {
                out.refuse(
                    connection,
                    "Pick a base to address before writing into the arena.",
                );
                return;
            }
        };

        if to == Audience::Channel(heard_in) {
            out.relay(connection, to, text);
        } else {
            out.refuse(connection, "You can only write where you are heard.");
        }
    }

    fn requested(&mut self, connection: ConnectionId, channel: ChannelKey, out: &mut Reply) {
        if channel == BACK {
            // Remember what they were before the lobby takes them back, so a
            // round trip does not silently demote a player to a spectator.
            let intent = match self.roles.get(&connection) {
                Some(Role::Player(side)) => Intent::Join(*side),
                _ => Intent::Spectate,
            };
            self.chosen.set(connection, intent);
            match self.destinations.lobby() {
                Some(lobby) => out.switch(connection, lobby),
                None => out.refuse(connection, "The lobby is not running."),
            }
            return;
        }

        let Some(role) = self.roles.get(&connection).copied() else {
            return;
        };

        let next = match role {
            // For staff, picking a base is not joining it: it is choosing who to
            // address. Everything else silences them again.
            Role::Admin { .. } => Role::Admin {
                addressing: match channel {
                    RED_BASE => Some(Side::Red),
                    BLUE_BASE => Some(Side::Blue),
                    _ => None,
                },
            },
            _ => match channel {
                RED_BASE => Role::Player(Side::Red),
                BLUE_BASE => Role::Player(Side::Blue),
                // The deck and neutral ground both mean "no team". A player can
                // always reach neutral ground: it sits at the root scope, which
                // every observation is comparable to.
                DECK | NEUTRAL => Role::Spectator,
                _ => role,
            },
        };

        self.roles.insert(connection, next);
    }
}
