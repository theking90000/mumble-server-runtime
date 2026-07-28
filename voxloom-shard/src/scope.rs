//! Scopes: the shared-view visibility mechanism.
//!
//! A scope is a path in a tree. Every rendered element occupies one; every
//! connection observes a small set of them. Visibility is a single line:
//!
//! > a connection sees an element when one of its observed scopes is
//! > **comparable** to the element's, meaning one is a prefix of the other.
//!
//! Both directions count, and that is what makes the relation useful: a player
//! at `/g7/t2` sees its ancestors (`/`, `/g7`) *and* its descendants, while a
//! spectator at `/g7` sees every team without anything being added to the model.
//! Moving up the tree is how you see more.
//!
//! # The closure theorem
//!
//! Imposing one rule on rendering - a child channel's scope **extends** its
//! parent's, a user's scope extends its channel's - makes the property everything
//! else depends on automatic: *if I see an element, I see what it refers to*.
//!
//! With `c` the channel's scope and `u` the user's, `c` a prefix of `u`, an
//! observer seeing the user through some scope `s` comparable to `u`:
//!
//! - `s` is a prefix of `u`: then `c` is also a prefix of `u`, so `s` and `c` are
//!   two prefixes of the same path, hence comparable;
//! - `u` is a prefix of `s`: then `c` prefixes `u` prefixes `s`.
//!
//! Either way the observer sees the channel. There is no runtime check to write
//! and no failure mode to chase, provided the builder can only ever narrow -
//! which is why [`Scope::child`] is the only way to make a new one.
//!
//! REF: docs/design/guide-implementation.md 2

/// How deep a scope path may go.
///
/// Four is a starting point, not a truth (guide 17.3). It is a hard bound
/// because [`Scope`] is `Copy` and consulted once per connection per turn: a
/// heap-allocated path here would put an allocation on the composition path.
pub const MAX_DEPTH: usize = 4;

/// How many scopes one connection may observe at once.
pub const MAX_OBSERVED: usize = 4;

/// A position in the visibility tree.
///
/// Segments are opaque to Mumble Server Runtime: a flavor puts whatever it wants in them (a
/// game id, a team id). Two scopes are equal when their paths are equal, so this
/// derives `Ord` for use as a map key and for canonicalizing a [`ScopeSet`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Scope {
    segments: [u32; MAX_DEPTH],
    depth: u8,
}

impl Scope {
    /// The root scope: what everyone who observes anything at all can see.
    pub const ROOT: Scope = Scope {
        segments: [0; MAX_DEPTH],
        depth: 0,
    };

    /// Extend this scope by one segment.
    ///
    /// This is the only constructor beyond [`Scope::ROOT`], and it can only ever
    /// narrow. That is what makes an incoherent view inexpressible rather than
    /// merely invalid: there is no free scope parameter anywhere in the builder.
    ///
    /// Returns `None` past [`MAX_DEPTH`]. Saturating instead would silently give
    /// the child its parent's scope, widening what the flavor asked to restrict -
    /// a privacy leak dressed as a rounding error. Refusing propagates into a
    /// build error that keeps the previous view (guide 11.7).
    #[must_use]
    pub fn child(self, segment: u32) -> Option<Scope> {
        let depth = usize::from(self.depth);
        if depth >= MAX_DEPTH {
            return None;
        }
        let mut segments = self.segments;
        // Bounded by the check above, so the index is in range.
        segments[depth] = segment;
        Some(Scope {
            segments,
            // `depth < MAX_DEPTH <= u8::MAX`, so this cannot overflow.
            depth: self.depth.saturating_add(1),
        })
    }

    /// How many segments this scope carries. The root has zero.
    #[must_use]
    pub fn depth(self) -> usize {
        usize::from(self.depth)
    }

    /// Whether this scope is a prefix of `other` (a scope is its own prefix).
    #[must_use]
    pub fn is_prefix_of(self, other: Scope) -> bool {
        let depth = usize::from(self.depth);
        depth <= usize::from(other.depth) && self.segments[..depth] == other.segments[..depth]
    }

    /// Whether either scope is a prefix of the other.
    ///
    /// Reflexive and symmetric by construction. Deliberately **not** transitive:
    /// `/g7` is comparable to both `/g7/t2` and `/g7/t3`, which are not
    /// comparable to each other. That is the point, not a defect.
    #[must_use]
    pub fn comparable(self, other: Scope) -> bool {
        self.is_prefix_of(other) || other.is_prefix_of(self)
    }
}

/// A connection dropped more observed scopes than [`MAX_OBSERVED`] allows.
///
/// `observation()` runs once per connection per turn, so its result has to stay
/// a two-word `Copy` value; letting it grow is how the composition cost goes
/// quadratic again (guide 3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a connection may observe at most {MAX_OBSERVED} scopes, {requested} were given")]
pub struct TooManyScopes {
    pub requested: usize,
}

/// What one connection observes of the shared view.
///
/// Canonicalized on construction (sorted, deduplicated) so that equality is set
/// equality. The shard compares the freshly asked-for observation against the
/// committed one to decide whether a connection needs the slow path, and a
/// flavor that returns the same set in a different order must not be mistaken
/// for one that moved.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ScopeSet {
    scopes: [Option<Scope>; MAX_OBSERVED],
}

impl ScopeSet {
    /// Observes nothing at all. This is the state a connection is attached in,
    /// which is what makes attaching an ordinary scope change (guide 9.6).
    pub const NONE: ScopeSet = ScopeSet {
        scopes: [None; MAX_OBSERVED],
    };

    /// Build an observation from a slice of scopes.
    ///
    /// # Errors
    ///
    /// [`TooManyScopes`] when more than [`MAX_OBSERVED`] *distinct* scopes are
    /// given. Duplicates are free.
    pub fn new(scopes: &[Scope]) -> Result<ScopeSet, TooManyScopes> {
        let mut sorted: Vec<Scope> = scopes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        if sorted.len() > MAX_OBSERVED {
            return Err(TooManyScopes {
                requested: sorted.len(),
            });
        }

        let mut set = ScopeSet::NONE;
        for (slot, scope) in set.scopes.iter_mut().zip(sorted) {
            *slot = Some(scope);
        }
        Ok(set)
    }

    /// Whether an element at `element` is visible to this connection.
    #[must_use]
    pub fn sees(self, element: Scope) -> bool {
        self.scopes
            .iter()
            .flatten()
            .any(|observed| element.comparable(*observed))
    }

    /// Whether this connection observes nothing.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.scopes.iter().all(Option::is_none)
    }

    /// The observed scopes, in canonical order.
    pub fn iter(self) -> impl Iterator<Item = Scope> {
        self.scopes.into_iter().flatten()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn scope(segments: &[u32]) -> Scope {
        let mut scope = Scope::ROOT;
        for segment in segments {
            scope = scope.child(*segment).expect("within MAX_DEPTH");
        }
        scope
    }

    #[test]
    fn a_child_is_never_a_strict_prefix_of_its_parent() {
        let parent = scope(&[7]);
        let child = parent.child(2).expect("within MAX_DEPTH");

        assert!(parent.is_prefix_of(child));
        assert!(!child.is_prefix_of(parent), "child must not widen");
        assert_eq!(child.depth(), parent.depth() + 1);
    }

    #[test]
    fn comparable_is_reflexive_and_symmetric() {
        let paths = [
            scope(&[]),
            scope(&[7]),
            scope(&[7, 2]),
            scope(&[7, 3]),
            scope(&[8]),
        ];

        for left in paths {
            assert!(left.comparable(left), "reflexive");
            for right in paths {
                assert_eq!(
                    left.comparable(right),
                    right.comparable(left),
                    "symmetric for {left:?} and {right:?}"
                );
            }
        }
    }

    #[test]
    fn siblings_are_not_comparable_but_share_an_ancestor() {
        let team_two = scope(&[7, 2]);
        let team_three = scope(&[7, 3]);
        let game = scope(&[7]);

        assert!(!team_two.comparable(team_three), "teams must be isolated");
        assert!(game.comparable(team_two));
        assert!(game.comparable(team_three));
    }

    #[test]
    fn observing_higher_sees_strictly_more() {
        let player = ScopeSet::new(&[scope(&[7, 2])]).expect("one scope");
        let spectator = ScopeSet::new(&[scope(&[7])]).expect("one scope");

        // The player sees its own team and every ancestor, but not the sibling.
        assert!(player.sees(scope(&[7, 2])));
        assert!(player.sees(scope(&[7])));
        assert!(player.sees(scope(&[])));
        assert!(!player.sees(scope(&[7, 3])));

        // The spectator sees both teams without anything being added to the model.
        assert!(spectator.sees(scope(&[7, 2])));
        assert!(spectator.sees(scope(&[7, 3])));
        assert!(!spectator.sees(scope(&[8])));
    }

    #[test]
    fn narrowing_past_the_bound_refuses_instead_of_widening() {
        let deepest = scope(&[1, 2, 3, 4]);
        assert_eq!(deepest.depth(), MAX_DEPTH);
        assert_eq!(
            deepest.child(5),
            None,
            "saturating here would hand the child its parent's visibility"
        );
    }

    #[test]
    fn an_observation_is_canonical_so_equality_is_set_equality() {
        let ordered = ScopeSet::new(&[scope(&[7]), scope(&[8])]).expect("two scopes");
        let reversed = ScopeSet::new(&[scope(&[8]), scope(&[7])]).expect("two scopes");
        let duplicated =
            ScopeSet::new(&[scope(&[8]), scope(&[7]), scope(&[8])]).expect("two distinct scopes");

        assert_eq!(ordered, reversed, "order must not look like a move");
        assert_eq!(ordered, duplicated, "duplicates must not look like a move");
    }

    #[test]
    fn too_many_distinct_scopes_are_refused() {
        let many: Vec<Scope> = (0..=u32::try_from(MAX_OBSERVED).unwrap_or(u32::MAX))
            .map(|segment| scope(&[segment]))
            .collect();

        assert_eq!(
            ScopeSet::new(&many),
            Err(TooManyScopes {
                requested: many.len()
            })
        );
    }

    #[test]
    fn observing_nothing_sees_nothing() {
        assert!(ScopeSet::NONE.is_empty());
        assert!(!ScopeSet::NONE.sees(Scope::ROOT));
    }
}
