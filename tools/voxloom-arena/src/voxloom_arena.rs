//! A demo flavor that uses all three visibility mechanisms at once.
//!
//! The runtime offers three, and the guide is emphatic that wanting to unify
//! them is the mistake that breeds special cases. So this flavor uses each for
//! what it is for, and the whole thing is driven from a stock Mumble client by
//! double-clicking channels.
//!
//! | mechanism | here |
//! |---|---|
//! | shared view + scopes | the two teams: a red player sees red, and cannot see that blue exists |
//! | private overlay | the vanished admin, and the private channel only they hold |
//! | audio relation | spectators hear both teams and are heard by neither; the admin addresses one team |
//!
//! # The two shards
//!
//! ```text
//!   lobby                              arena
//!   Voxloom Arena                      The Arena
//!   |- Red Team                        |- Red Base          scope [1]
//!   |- Blue Team                       |- Blue Base         scope [2]
//!   |- Spectators                      |- Observation Deck  scope [3]
//!   `- > Enter the Arena  -----------> |- Neutral Ground    scope ROOT
//!                          <---------- `- < Back to the Lobby
//! ```
//!
//! Everything a player does is a channel request. Entering the arena is a
//! migration; stepping onto Neutral Ground makes a player a spectator, which
//! widens their observation to the whole tree; picking a base from there narrows
//! it again. Those are the guide's change classes 2 and 3, reachable by hand.
//!
//! # The admin
//!
//! An admin is invisible: they are in no shared channel, and no other connection
//! renders them. They still hear everything, through `audio_listen` on both team
//! domains, which is one-way by construction.
//!
//! When they address a team, that team - and only that team - is given an
//! overlay entry for them, and a directed audio edge is opened to each member.
//! The two go together and cannot be separated: a receiver must see its sender
//! or the client will discard the audio, and the render refuses to build if that
//! is not true. This is the one place where the coupling the guide calls "the
//! only one, and it is not ours" becomes visible in business code.
#![forbid(unsafe_code)]

pub mod arena;
pub mod directory;
pub mod lobby;
pub mod router;

pub use arena::{Arena, Role, Side};
pub use directory::{Destinations, Directory, Member};
pub use lobby::Lobby;
pub use router::ArenaRouter;
