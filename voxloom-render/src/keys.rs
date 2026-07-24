//! Semantic view keys.
//!
//! A view key is the stable semantic identity of a rendered element, equivalent
//! to a React `key`: it survives across renders even as the numeric view id is
//! (re)allocated. The reconciler maps keys to ids and keeps that mapping stable
//! per connection (ADR-007).
//!
//! REF: docs/voxloom-specification-technique-v0.1.md 6 ("View key"), 8.4 and 34
//!      (`ChannelKey::Static("connected")`, `ChannelKey::Match(game.id)`,
//!      `ActionKey::Static("invite-party")`).

/// The stable identity carried by every view key.
///
/// `Static` names a semantically fixed element (a channel that always exists for
/// a connection); `Dynamic` names an element derived from a canonical entity
/// (a game, a party, a player), carried as an opaque id so this crate stays
/// free of any domain or wire meaning.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticKey {
    /// A fixed, human-authored identity (spec example: `Static("connected")`).
    Static(String),
    /// An identity derived from a canonical entity id (spec example: `Match(game.id)`).
    Dynamic(u64),
}

/// Semantic key of a channel element. Distinct from [`UserKey`] and [`ActionKey`]
/// so the type system prevents cross-referencing a channel where a user is meant
/// (spec risk 5: types must forbid view/entity confusion).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelKey(pub SemanticKey);

/// Semantic key of a user element.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UserKey(pub SemanticKey);

/// Semantic key of a context action element.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActionKey(pub SemanticKey);
