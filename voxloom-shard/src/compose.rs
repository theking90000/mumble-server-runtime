//! Composing one connection's transition out of the shared delta.
//!
//! This is the heart of the runtime, and it is about fifteen lines:
//!
//! ```text
//! let shared  = filter(journal.replay(cursor, head), see);
//! let private = plan_elements(&overlay_sent, &overlay_new);
//! let mut ops = splice(shared, private);
//! collapse(&mut ops);
//! ```
//!
//! # What must not be done instead
//!
//! `diff(overlay + shared, committed)` is the honest formulation and it is always
//! correct - and it costs **O(V) per connection**, so O(N·W) overall. That is
//! precisely the quadratic this design exists to escape. Composition costs
//! `O(|D|) + O(|overlay|) + O(|ops|)`.
//!
//! REF: docs/design/guide-implementation.md 6

use std::collections::HashSet;

use crate::plan::{ElementId, OverlayOps, PlanOp, PlannedOp};
use crate::scope::ScopeSet;

/// Keep the operations this connection can see.
///
/// **Filtering preserves validity for free.** Every ordering rule in the plan is
/// a constraint of the form "X before Y", and dropping elements from a sequence
/// violates none of them. So a valid plan, filtered, is still a valid plan and
/// there is nothing to replan. The closure theorem does the real work: it is what
/// guarantees no reference is left dangling by the elements that were dropped.
///
/// Takes anything that iterates borrowed operations, so a freshly planned
/// `Vec<PlannedOp>` and a `Vec<&PlannedOp>` replayed out of the journal both go
/// through the same function.
#[must_use]
pub fn filter<'a>(ops: impl IntoIterator<Item = &'a PlannedOp>, see: ScopeSet) -> Vec<PlanOp> {
    ops.into_iter()
        .filter(|planned| see.sees(planned.scope))
        .map(|planned| planned.op.clone())
        .collect()
}

/// Insert the overlay's operations **inside** the shared phases.
///
/// The overlay does not append. The counter-example is immediate: suppose the
/// shared delta removes channel C, and the overlay had placed the admin in it.
/// With the private operations last, `RemoveChannel(C)` goes out *before* the
/// admin is withdrawn, and an occupied channel is deleted.
///
/// So the insertion point is the boundary between the shared additions
/// (P1 to P5) and the shared removals (P6, P7):
///
/// ```text
/// P1..P5   shared additions, filtered
///          ├─ overlay removals      <- before: they vacate a channel about to die
///          └─ overlay additions     <- after:  they may target a brand new channel
/// P6..P7   shared removals, filtered
/// ```
#[must_use]
pub fn splice(shared: Vec<PlanOp>, private: OverlayOps) -> Vec<PlanOp> {
    let boundary = shared
        .iter()
        .position(PlanOp::is_removal)
        .unwrap_or(shared.len());

    let mut composed =
        Vec::with_capacity(shared.len() + private.additions.len() + private.removals.len());
    let mut shared = shared;
    let tail = shared.split_off(boundary);
    composed.append(&mut shared);
    composed.extend(private.removals);
    composed.extend(private.additions);
    composed.extend(tail);
    composed
}

/// Drop every removal of an element that is also added in the same transition.
///
/// One pass, three jobs, because in all three the right answer is the same:
/// **an element that has an `Add` somewhere still exists, so the `Remove` is
/// wrong.** A full `AddUser`/`CreateChannel` carries the complete state, and the
/// client merges `UserState`/`ChannelState` rather than replacing, so re-sending
/// the element whole simply updates it.
///
/// | situation | operations produced | after `collapse` |
/// |---|---|---|
/// | the element changes scope | Remove(old) + Add(new) | Add alone |
/// | vanish to unvanish | Remove(overlay) + Add(shared) | Add alone, it changes channel |
/// | unvanish to vanish | Remove(shared) + Add(overlay) | Add alone |
/// | dropped from the overlay, absent from the shared view | Remove alone | Remove kept |
/// | gone everywhere | Remove alone | Remove kept |
///
/// Three problems that looked distinct, one function: that is the sign it sits
/// at the right level.
pub fn collapse(ops: &mut Vec<PlanOp>) {
    let added: HashSet<ElementId> = ops.iter().filter_map(PlanOp::added).collect();
    ops.retain(|op| !matches!(op.removed(), Some(id) if added.contains(&id)));
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::ids::{ChannelId, ChannelKey, ConnectionId, Occupant, SessionId};
    use crate::plan::{plan, plan_elements};
    use crate::scope::Scope;
    use crate::view::{Channel, Overlay, ShardView, User, UserFlags};

    fn scope(segments: &[u32]) -> Scope {
        let mut scope = Scope::ROOT;
        for segment in segments {
            scope = scope.child(*segment).expect("within MAX_DEPTH");
        }
        scope
    }

    fn channel(id: u32, parent: u32, scope: Scope) -> Channel {
        Channel {
            key: ChannelKey(u64::from(id)),
            id: ChannelId(id),
            parent: ChannelId(parent),
            scope,
            name: format!("channel-{id}"),
            position: 0,
            can_enter: true,
            can_text: true,
            links: BTreeSet::new(),
        }
    }

    fn user(session: u32, channel: u32, scope: Scope) -> User {
        User {
            occupant: Occupant::Connection(ConnectionId(u64::from(session))),
            session: SessionId(session),
            channel: ChannelId(channel),
            scope,
            name: format!("user-{session}"),
            flags: UserFlags::default(),
        }
    }

    /// Root, game, two teams, one player in each.
    fn two_teams() -> ShardView {
        let mut view = ShardView::empty();
        view.channels
            .insert(ChannelId::ROOT, channel(0, 0, Scope::ROOT));
        view.channels
            .insert(ChannelId(1), channel(1, 0, scope(&[7])));
        view.channels
            .insert(ChannelId(2), channel(2, 1, scope(&[7, 2])));
        view.channels
            .insert(ChannelId(3), channel(3, 1, scope(&[7, 3])));
        view.users
            .insert(SessionId(10), user(10, 2, scope(&[7, 2])));
        view.users
            .insert(SessionId(11), user(11, 3, scope(&[7, 3])));
        view
    }

    fn sessions_touched(ops: &[PlanOp]) -> Vec<(&'static str, u32)> {
        ops.iter()
            .filter_map(|op| match op {
                PlanOp::AddUser(user) => Some(("add", user.session.0)),
                PlanOp::RemoveUser(session) => Some(("remove", session.0)),
                PlanOp::MoveUser { session, .. } => Some(("move", session.0)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_scope_change_reaches_each_observer_as_the_half_that_concerns_it() {
        // Player 11 leaves team 3 for team 2.
        let before = two_teams();
        let mut after = before.clone();
        after
            .users
            .insert(SessionId(11), user(11, 2, scope(&[7, 2])));
        let ops = plan(&before, &after);

        let team_three = ScopeSet::new(&[scope(&[7, 3])]).expect("one scope");
        let team_two = ScopeSet::new(&[scope(&[7, 2])]).expect("one scope");
        let spectator = ScopeSet::new(&[scope(&[7])]).expect("one scope");
        let elsewhere = ScopeSet::new(&[scope(&[8])]).expect("one scope");

        // The teammate left behind only learns of the departure.
        assert_eq!(
            sessions_touched(&filter(&ops, team_three)),
            vec![("remove", 11)]
        );
        // The new team only learns of the arrival.
        assert_eq!(sessions_touched(&filter(&ops, team_two)), vec![("add", 11)]);
        // Another game learns nothing at all.
        assert!(filter(&ops, elsewhere).is_empty());

        // The spectator sees both halves, and collapse settles it into a move.
        let mut both = filter(&ops, spectator);
        assert_eq!(
            sessions_touched(&both),
            vec![("add", 11), ("remove", 11)],
            "the raw filter yields both halves"
        );
        collapse(&mut both);
        assert_eq!(
            sessions_touched(&both),
            vec![("add", 11)],
            "a full AddUser merges client-side, which is the move"
        );
    }

    #[test]
    fn a_move_within_one_scope_stays_a_move() {
        let before = two_teams();
        let mut after = before.clone();
        // Same scope, different channel: a plain relocation, not a departure.
        after
            .users
            .insert(SessionId(10), user(10, 1, scope(&[7, 2])));

        let ops = plan(&before, &after);
        let team_two = ScopeSet::new(&[scope(&[7, 2])]).expect("one scope");
        assert_eq!(
            sessions_touched(&filter(&ops, team_two)),
            vec![("move", 10)]
        );
    }

    #[test]
    fn filtering_preserves_order() {
        let before = two_teams();
        let mut after = before.clone();
        after
            .channels
            .insert(ChannelId(4), channel(4, 2, scope(&[7, 2])));
        after.users.remove(&SessionId(10));

        let ops = plan(&before, &after);
        let everything = ScopeSet::new(&[Scope::ROOT]).expect("one scope");
        let filtered = filter(&ops, everything);

        let unfiltered: Vec<&PlanOp> = ops.iter().map(|planned| &planned.op).collect();
        let kept: Vec<&PlanOp> = filtered.iter().collect();
        assert_eq!(unfiltered, kept, "an unrestricted filter is the identity");

        // And a real filter is a subsequence, never a reordering.
        let team_two = ScopeSet::new(&[scope(&[7, 2])]).expect("one scope");
        let restricted = filter(&ops, team_two);
        let mut remaining = restricted.iter();
        for op in &ops {
            if remaining.clone().next() == Some(&op.op) {
                let _ = remaining.next();
            }
        }
        assert_eq!(remaining.count(), 0, "filtering must not reorder");
    }

    #[test]
    fn the_overlay_is_spliced_before_the_shared_removals() {
        // The shared delta deletes the channel the overlay was using.
        let shared = vec![
            PlanOp::CreateChannel(channel(5, 0, Scope::ROOT)),
            PlanOp::RemoveChannel(ChannelId(2)),
        ];
        let private = OverlayOps {
            additions: vec![PlanOp::AddUser(user(99, 5, Scope::ROOT))],
            removals: vec![PlanOp::RemoveUser(SessionId(98))],
        };

        let composed = splice(shared, private);
        let shapes: Vec<&str> = composed
            .iter()
            .map(|op| match op {
                PlanOp::CreateChannel(_) => "create-channel",
                PlanOp::AddUser(_) => "add-user",
                PlanOp::RemoveUser(_) => "remove-user",
                PlanOp::RemoveChannel(_) => "remove-channel",
                _ => "other",
            })
            .collect();

        assert_eq!(
            shapes,
            vec![
                "create-channel",
                "remove-user",
                "add-user",
                "remove-channel"
            ],
            "the overlay's withdrawal must precede the shared channel removal"
        );
    }

    #[test]
    fn an_overlay_addition_may_target_a_channel_created_in_the_same_turn() {
        let shared = vec![PlanOp::CreateChannel(channel(5, 0, Scope::ROOT))];
        let private = OverlayOps {
            additions: vec![PlanOp::AddUser(user(99, 5, Scope::ROOT))],
            removals: Vec::new(),
        };

        let composed = splice(shared, private);
        let create = composed
            .iter()
            .position(|op| matches!(op, PlanOp::CreateChannel(_)))
            .expect("the creation is present");
        let add = composed
            .iter()
            .position(|op| matches!(op, PlanOp::AddUser(_)))
            .expect("the addition is present");
        assert!(create < add);
    }

    #[test]
    fn vanishing_and_unvanishing_both_collapse_to_a_single_addition() {
        let admin = Occupant::Connection(ConnectionId(99));
        let placed = User {
            occupant: admin,
            session: SessionId(99),
            channel: ChannelId(2),
            scope: scope(&[7, 2]),
            name: "admin".to_owned(),
            flags: UserFlags::default(),
        };

        // unvanish: the overlay drops them, the shared view picks them up.
        let mut overlay_before = Overlay::default();
        overlay_before.users.insert(SessionId(99), placed.clone());
        let private = plan_elements(&overlay_before, &Overlay::default());
        let mut ops = splice(vec![PlanOp::AddUser(placed.clone())], private);
        collapse(&mut ops);
        assert_eq!(sessions_touched(&ops), vec![("add", 99)]);

        // vanish: the shared view drops them, the overlay picks them up.
        let mut overlay_after = Overlay::default();
        overlay_after.users.insert(SessionId(99), placed);
        let private = plan_elements(&Overlay::default(), &overlay_after);
        let mut ops = splice(vec![PlanOp::RemoveUser(SessionId(99))], private);
        collapse(&mut ops);
        assert_eq!(sessions_touched(&ops), vec![("add", 99)]);
    }

    #[test]
    fn a_removal_with_no_matching_addition_survives_collapse() {
        let mut ops = vec![
            PlanOp::RemoveUser(SessionId(10)),
            PlanOp::RemoveChannel(ChannelId(2)),
        ];
        collapse(&mut ops);
        assert_eq!(ops.len(), 2, "an element that is gone must stay gone");
    }

    #[test]
    fn splicing_into_an_all_removal_plan_still_lands_before_them() {
        let shared = vec![PlanOp::RemoveUser(SessionId(10))];
        let private = OverlayOps {
            additions: vec![PlanOp::AddUser(user(99, 2, Scope::ROOT))],
            removals: Vec::new(),
        };
        let composed = splice(shared, private);
        assert!(matches!(composed[0], PlanOp::AddUser(_)));
    }

    #[test]
    fn an_empty_overlay_leaves_the_shared_plan_untouched() {
        let shared = vec![
            PlanOp::CreateChannel(channel(5, 0, Scope::ROOT)),
            PlanOp::RemoveUser(SessionId(10)),
        ];
        let composed = splice(shared.clone(), OverlayOps::default());
        assert_eq!(composed, shared);
    }

    #[test]
    fn an_unobserved_delta_filters_to_nothing() {
        let before = two_teams();
        let mut after = before.clone();
        after.users.remove(&SessionId(10));
        let ops = plan(&before, &after);

        assert!(filter(&ops, ScopeSet::NONE).is_empty());
        assert!(BTreeMap::<u32, u32>::new().is_empty());
    }
}
