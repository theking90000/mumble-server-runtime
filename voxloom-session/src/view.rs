//! The committed view of one connection, and the rule that advances it.
//!
//! # Committed means delivered, not merely computed
//!
//! A connection's committed view is what the client can be assumed to hold. It
//! advances in one step, once the *whole* transition has been accepted
//! downstream (spec 12.7, invariant 20). Half a transition is never committed,
//! which is why [`ConnectionView::prepare`] hands back a [`PendingTransition`]
//! that has to be split, admitted, and only then turned in via
//! [`ConnectionView::commit`]. Dropping the token abandons the transition and
//! leaves the committed view exactly as it was — which is the correct outcome of
//! a refused admission, not an error path bolted on afterwards.
//!
//! # Why abandoning is cheap, and why intermediate states may be skipped
//!
//! The next attempt is planned from the *unchanged* committed view against
//! whatever the desired view has become in the meantime. The states that were
//! desired while the connection was congested are never replayed, and skipping
//! them is correct rather than merely economical: a transition is
//! `diff(committed, desired)`, a function of two states, not the replay of a
//! log. There is no missed event to catch up on, because there are no events.
//! Two rejected transitions that cancel each other out therefore cost nothing at
//! all — a property the tests pin down directly.
//!
//! This is also what keeps ADR-009 simple. Nothing is ever partially applied, so
//! nothing ever has to be partially rolled back.
//!
//! # What this module refuses
//!
//! An invalid desired view is refused **without** dropping the connection: the
//! committed view is still valid, and reconnecting would only produce the same
//! broken render again. The forced-reconnect case of ADR-009 belongs downstream,
//! where a transition can turn out to be undeliverable at all.

use std::collections::BTreeSet;

use thiserror::Error;
use voxloom_reconcile::{AudioRoute, ViewIdMapping, plan};
use voxloom_render::{ClientView, Invariant, SessionId, normalize, validate};

use crate::emit::{EmitError, EmittedStep, emit_transaction};

/// One connection's view lifecycle: what it shows, which ids it uses, and where
/// it is in the revision sequence.
#[derive(Debug)]
pub struct ConnectionView {
    self_session: SessionId,
    committed: ClientView,
    committed_routes: BTreeSet<AudioRoute>,
    ids: ViewIdMapping,
    revision: u64,
}

/// A transition that has been planned and translated but not yet delivered.
///
/// Splitting it is the only way to get at the steps, and the token that comes
/// with them is the only way to commit. Dropping either abandons the transition.
#[derive(Debug)]
#[must_use = "a prepared transition that is neither admitted nor dropped does nothing"]
pub struct PendingTransition {
    steps: Vec<EmittedStep>,
    token: CommitToken,
}

/// Proof that a specific transition was prepared, and the state it commits to.
///
/// It carries the revision it was planned from, so a token that has been
/// overtaken cannot be turned in later and quietly walk the view backwards.
#[derive(Debug)]
#[must_use = "dropping the token abandons the transition"]
pub struct CommitToken {
    from_revision: u64,
    to_revision: u64,
    next_view: ClientView,
    next_routes: BTreeSet<AudioRoute>,
}

/// Why a transition could not be prepared or committed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TransitionError {
    #[error("desired view violates invariant {invariant:?}: {detail}")]
    InvalidDesiredView {
        invariant: Invariant,
        detail: String,
    },

    #[error("desired view cannot be put on the wire: {0}")]
    Unemittable(#[from] EmitError),

    #[error(
        "commit token was planned from revision {expected} but the view is at {found}; refusing \
         rather than replacing a newer view with an older one"
    )]
    StaleCommit { expected: u64, found: u64 },
}

impl ConnectionView {
    /// A connection that has been told nothing yet.
    pub fn new(self_session: SessionId) -> ConnectionView {
        // `ClientView::empty()` is the minimal *valid* view and contains the
        // root. A brand-new wire connection has not received even that root,
        // however. Keeping it in `committed` here would make the initial diff
        // omit `CreateChannel(root)` and let the first synthetic child precede
        // its parent. The pre-sync shadow state is therefore deliberately
        // uninitialized; the first desired view is validated, creates root
        // first, and the first commit makes `committed` valid.
        let mut committed = ClientView::empty();
        committed.channels.clear();
        ConnectionView {
            self_session,
            committed,
            committed_routes: BTreeSet::new(),
            ids: ViewIdMapping::new(),
            revision: 0,
        }
    }

    /// What the client is assumed to hold.
    pub fn committed(&self) -> &ClientView {
        &self.committed
    }

    /// The audio routes that go with the committed view. They advance together,
    /// so a route can never be authorized against a view the client lacks.
    pub fn committed_routes(&self) -> &BTreeSet<AudioRoute> {
        &self.committed_routes
    }

    /// The revision of the committed view. Monotonic, and only ever moved by a
    /// successful [`ConnectionView::commit`].
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// This connection's own session.
    pub fn self_session(&self) -> SessionId {
        self.self_session
    }

    /// The key-to-id mapping (ADR-007). Exposed because building a desired view
    /// means resolving keys through it, and because the inbound direction has to
    /// resolve a client-supplied id back to a key before it may act on it.
    pub fn ids(&self) -> &ViewIdMapping {
        &self.ids
    }

    /// Mutable access, for the allocating direction of the mapping.
    pub fn ids_mut(&mut self) -> &mut ViewIdMapping {
        &mut self.ids
    }

    /// Plan and translate the move to `desired`, without changing anything.
    ///
    /// `Ok(None)` means the connection is already showing that view: an empty
    /// transition is not sent, so a render that changed nothing costs one diff
    /// and no traffic.
    ///
    /// The desired view is normalized and validated first (spec 12.2), so the
    /// planner and the emitter only ever see a view that satisfies the
    /// invariants. A view that does not is refused here, before a single frame
    /// is built.
    pub fn prepare(
        &self,
        desired: &ClientView,
        desired_routes: &BTreeSet<AudioRoute>,
    ) -> Result<Option<PendingTransition>, TransitionError> {
        let desired = normalize(desired);
        validate(&desired, Some(self.self_session)).map_err(|error| {
            TransitionError::InvalidDesiredView {
                invariant: error.invariant,
                detail: error.detail,
            }
        })?;

        let to_revision = self.revision.saturating_add(1);
        let transaction = plan(
            &self.committed,
            &desired,
            &self.committed_routes,
            desired_routes,
            self.revision,
            to_revision,
        );

        let steps = emit_transaction(&transaction, &self.committed, self.self_session)?;
        if steps.is_empty() {
            return Ok(None);
        }

        Ok(Some(PendingTransition {
            steps,
            token: CommitToken {
                from_revision: self.revision,
                to_revision,
                next_view: transaction.next_view,
                next_routes: desired_routes.clone(),
            },
        }))
    }

    /// Advance the committed view. Call only once every step of the transition
    /// has been accepted downstream.
    ///
    /// Ids of channels that disappear are released here rather than at planning
    /// time: until the transition is delivered, the client still holds them, and
    /// a key resolved in between must keep getting the id the client knows.
    /// Released ids are retired for the life of the connection (invariant 12).
    pub fn commit(&mut self, token: CommitToken) -> Result<(), TransitionError> {
        if token.from_revision != self.revision {
            return Err(TransitionError::StaleCommit {
                expected: token.from_revision,
                found: self.revision,
            });
        }

        for channel in self.committed.channels.values() {
            if !token.next_view.channels.contains_key(&channel.id) {
                self.ids.release(&channel.key);
            }
        }

        self.committed = token.next_view;
        self.committed_routes = token.next_routes;
        self.revision = token.to_revision;
        Ok(())
    }
}

impl PendingTransition {
    /// The steps, in the order they must be admitted.
    pub fn steps(&self) -> &[EmittedStep] {
        &self.steps
    }

    /// Split into the steps to admit and the token that commits them.
    ///
    /// Taking the steps by value avoids cloning a transition's worth of frames
    /// just to hand them to the output queue.
    pub fn split(self) -> (Vec<EmittedStep>, CommitToken) {
        (self.steps, self.token)
    }
}

impl CommitToken {
    /// The revision this transition commits to.
    pub fn to_revision(&self) -> u64 {
        self.to_revision
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use voxloom_protocol::ControlMessage;
    use voxloom_render::{ChannelId, ChannelKey, SemanticKey, UserKey, ViewChannel, ViewUser};

    const SELF: SessionId = SessionId(1);

    fn channel(id: u32, name: &str) -> ViewChannel {
        ViewChannel {
            key: ChannelKey(SemanticKey::Static(name.to_string())),
            id: ChannelId(id),
            parent: ChannelId::ROOT,
            name: name.to_string(),
            description: None,
            position: 0,
            temporary: false,
            max_users: None,
            enter_restricted: false,
            can_enter: true,
            links: BTreeSet::new(),
        }
    }

    fn self_user() -> ViewUser {
        ViewUser {
            key: UserKey(SemanticKey::Static("self".to_string())),
            session: SELF,
            name: "alice".to_string(),
            channel: ChannelId::ROOT,
            user_id: None,
            certificate_hash: None,
            mute: false,
            deaf: false,
            suppress: false,
            self_mute: false,
            self_deaf: false,
            priority_speaker: false,
            recording: false,
            comment: None,
            texture: None,
        }
    }

    /// A valid view: the root, the self user in it, plus the named channels.
    fn view(extra: &[&str]) -> ClientView {
        let mut channels = BTreeMap::from([(ChannelId::ROOT, channel(0, "root"))]);
        for (index, name) in extra.iter().enumerate() {
            let id = u32::try_from(index).expect("test index fits") + 1;
            channels.insert(ChannelId(id), channel(id, name));
        }
        ClientView {
            channels,
            users: BTreeMap::from([(SELF, self_user())]),
            ..ClientView::empty()
        }
    }

    fn no_routes() -> BTreeSet<AudioRoute> {
        BTreeSet::new()
    }

    fn created_channel_names(steps: &[EmittedStep]) -> Vec<String> {
        steps
            .iter()
            .filter_map(|step| match step {
                EmittedStep::Message(ControlMessage::ChannelState(state)) => state.name.clone(),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn preparing_changes_nothing_until_the_transition_is_committed() {
        let mut connection = ConnectionView::new(SELF);

        let pending = connection
            .prepare(&view(&["team"]), &no_routes())
            .expect("view is valid")
            .expect("something to send");

        // A fresh wire connection has been told literally nothing yet, not even
        // about the root. Preparing must not have added any of it.
        assert!(
            connection.committed().users.is_empty(),
            "the client has been told nothing yet"
        );
        assert!(connection.committed().channels.is_empty());
        assert_eq!(connection.revision(), 0);

        let (_steps, token) = pending.split();
        connection.commit(token).expect("fresh token");

        assert_eq!(connection.committed().channels.len(), 2);
        assert_eq!(connection.revision(), 1);
    }

    #[test]
    fn an_abandoned_transition_leaves_the_committed_view_untouched() {
        let mut connection = ConnectionView::new(SELF);
        let pending = connection
            .prepare(&view(&[]), &no_routes())
            .expect("view is valid")
            .expect("something to send");
        drop(pending);

        assert!(connection.committed().users.is_empty());
        assert_eq!(connection.revision(), 0);

        // The connection is still able to make progress afterwards.
        let retry = connection
            .prepare(&view(&[]), &no_routes())
            .expect("view is valid")
            .expect("something to send");
        let (_steps, token) = retry.split();
        connection.commit(token).expect("fresh token");
        assert_eq!(connection.revision(), 1);
    }

    #[test]
    fn intermediate_states_that_cancel_out_cost_nothing() {
        let mut connection = ConnectionView::new(SELF);
        let (_steps, token) = connection
            .prepare(&view(&[]), &no_routes())
            .expect("view is valid")
            .expect("something to send")
            .split();
        connection.commit(token).expect("fresh token");

        // A transition is refused, as a congested queue would refuse it.
        let refused = connection
            .prepare(&view(&["team"]), &no_routes())
            .expect("view is valid")
            .expect("something to send");
        drop(refused);

        // By the time we retry, the desired view is back where it started. The
        // work that was refused is not replayed: there is nothing left to do.
        assert!(
            connection
                .prepare(&view(&[]), &no_routes())
                .expect("view is valid")
                .is_none(),
            "a refused transition must not survive as pending work"
        );
    }

    #[test]
    fn a_retry_plans_from_the_committed_view_to_the_newest_desired_one() {
        let mut connection = ConnectionView::new(SELF);
        let (_steps, token) = connection
            .prepare(&view(&[]), &no_routes())
            .expect("view is valid")
            .expect("something to send")
            .split();
        connection.commit(token).expect("fresh token");

        drop(
            connection
                .prepare(&view(&["red"]), &no_routes())
                .expect("view is valid")
                .expect("something to send"),
        );

        let retry = connection
            .prepare(&view(&["red", "blue"]), &no_routes())
            .expect("view is valid")
            .expect("something to send");

        // One transition carrying both creations, not a replay of the refused
        // one followed by a second.
        let names = created_channel_names(retry.steps());
        assert_eq!(names, vec!["red".to_string(), "blue".to_string()]);
    }

    #[test]
    fn a_disappearing_channel_retires_its_id() {
        let mut connection = ConnectionView::new(SELF);
        let key = ChannelKey(SemanticKey::Static("team".to_string()));

        // Bind the key to the id the view uses, as a real desired-view builder
        // would, then commit a view containing it.
        let bound = connection
            .ids_mut()
            .resolve(key.clone(), voxloom_reconcile::ChannelIdKind::Stable)
            .expect("id space is not exhausted");
        assert_eq!(bound, ChannelId(1));

        let (_steps, token) = connection
            .prepare(&view(&["team"]), &no_routes())
            .expect("view is valid")
            .expect("something to send")
            .split();
        connection.commit(token).expect("fresh token");
        assert_eq!(connection.ids().get(&key), Some(ChannelId(1)));

        // Removing it releases the id, and the next key gets a fresh one rather
        // than inheriting the client's stale cache (invariant 12).
        let (_steps, token) = connection
            .prepare(&view(&[]), &no_routes())
            .expect("view is valid")
            .expect("something to send")
            .split();
        connection.commit(token).expect("fresh token");

        assert_eq!(connection.ids().get(&key), None);
        let reused = connection
            .ids_mut()
            .resolve(
                ChannelKey(SemanticKey::Static("other".to_string())),
                voxloom_reconcile::ChannelIdKind::Stable,
            )
            .expect("id space is not exhausted");
        assert_ne!(reused, ChannelId(1), "a retired id must never come back");
    }

    #[test]
    fn an_invalid_desired_view_is_refused_without_losing_the_connection() {
        let mut connection = ConnectionView::new(SELF);
        let (_steps, token) = connection
            .prepare(&view(&[]), &no_routes())
            .expect("view is valid")
            .expect("something to send")
            .split();
        connection.commit(token).expect("fresh token");

        // A child whose parent is not in the view breaks invariant 3.
        let mut broken = view(&[]);
        let mut orphan = channel(7, "orphan");
        orphan.parent = ChannelId(99);
        broken.channels.insert(ChannelId(7), orphan);

        let error = connection
            .prepare(&broken, &no_routes())
            .expect_err("an invalid view must not be planned");
        assert!(matches!(error, TransitionError::InvalidDesiredView { .. }));

        // The committed view survived, so the client is still consistent.
        assert_eq!(connection.revision(), 1);
        assert_eq!(connection.committed().channels.len(), 1);
    }

    #[test]
    fn a_token_that_has_been_overtaken_is_refused() {
        let mut connection = ConnectionView::new(SELF);

        let first = connection
            .prepare(&view(&["red"]), &no_routes())
            .expect("view is valid")
            .expect("something to send");
        let second = connection
            .prepare(&view(&["blue"]), &no_routes())
            .expect("view is valid")
            .expect("something to send");

        let (_steps, second_token) = second.split();
        connection.commit(second_token).expect("fresh token");

        // Committing the older token would walk the view backwards. It cannot
        // happen with a single owner, which is exactly why it is refused rather
        // than trusted.
        let (_steps, stale_token) = first.split();
        let error = connection
            .commit(stale_token)
            .expect_err("a stale token must not be honoured");
        assert_eq!(
            error,
            TransitionError::StaleCommit {
                expected: 0,
                found: 1
            }
        );
        assert_eq!(connection.revision(), 1);
    }

    #[test]
    fn a_view_that_did_not_change_produces_no_transition() {
        let mut connection = ConnectionView::new(SELF);
        let (_steps, token) = connection
            .prepare(&view(&["team"]), &no_routes())
            .expect("view is valid")
            .expect("something to send")
            .split();
        connection.commit(token).expect("fresh token");

        assert!(
            connection
                .prepare(&view(&["team"]), &no_routes())
                .expect("view is valid")
                .is_none(),
            "an unchanged render must cost no traffic"
        );
    }

    #[test]
    fn routes_advance_with_the_view_and_not_before_it() {
        let mut connection = ConnectionView::new(SELF);
        let route = AudioRoute {
            sender: SELF,
            receiver: SessionId(2),
        };
        let desired_routes = BTreeSet::from([route]);

        let pending = connection
            .prepare(&view(&[]), &desired_routes)
            .expect("view is valid")
            .expect("something to send");

        assert!(
            connection.committed_routes().is_empty(),
            "a route must not count as authorized before delivery"
        );

        let (steps, token) = pending.split();
        assert!(
            steps
                .iter()
                .any(|step| matches!(step, EmittedStep::RouteChange { enabled: true, .. })),
            "the new route has to be switched on as part of the transition"
        );

        connection.commit(token).expect("fresh token");
        assert_eq!(connection.committed_routes(), &desired_routes);
    }
}
