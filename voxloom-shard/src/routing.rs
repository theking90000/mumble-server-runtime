//! The audio relation, and the routing table compiled from it.
//!
//! Scopes drive the **shared visual** only. Audio is a directed relation the
//! flavor declares outright - one way, both ways, or not at all - because
//! "spectators hear the team without being heard" is not expressible as a tree
//! position, and forcing it to be is what makes visibility models grow special
//! cases.
//!
//! | primitive | cost | for |
//! |---|---|---|
//! | [`AudioRelation::domain`] | O(members squared) | the bulk: a channel, a team |
//! | [`AudioRelation::listen`] | O(members) | admin, spectator: hears without being heard |
//! | [`AudioRelation::edge`] | O(1) | full generality, at the flavor's cost |
//!
//! REF: docs/design/guide-implementation.md 3.5, 8.3

use std::collections::{BTreeMap, BTreeSet};

use crate::ids::{ConnectionId, SessionId};

/// A named group of connections that all hear each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DomainId(pub u64);

/// The directed "who may hear whom" relation, as the flavor declared it.
///
/// Accumulated during a render and compiled once at the end. Kept in terms of
/// [`ConnectionId`] because that is the vocabulary a flavor thinks in; the
/// translation to [`SessionId`] happens at compile time, against the view.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioRelation {
    domains: BTreeMap<DomainId, BTreeSet<ConnectionId>>,
    listeners: BTreeSet<(ConnectionId, DomainId)>,
    edges: BTreeSet<(ConnectionId, ConnectionId)>,
}

impl AudioRelation {
    /// Declare a symmetric group: every member hears every other member.
    ///
    /// Repeated calls with the same domain accumulate members rather than
    /// replacing them, so a flavor may build a domain across several loops.
    pub fn domain(&mut self, domain: DomainId, members: &[ConnectionId]) {
        self.domains
            .entry(domain)
            .or_default()
            .extend(members.iter().copied());
    }

    /// Declare a one-way exception: `listener` hears the domain, and is not
    /// heard by it.
    pub fn listen(&mut self, listener: ConnectionId, domain: DomainId) {
        self.listeners.insert((listener, domain));
    }

    /// Declare a single directed edge.
    pub fn edge(&mut self, sender: ConnectionId, receiver: ConnectionId) {
        self.edges.insert((sender, receiver));
    }

    /// The members of each declared domain.
    pub fn domains(&self) -> impl Iterator<Item = (DomainId, &BTreeSet<ConnectionId>)> {
        self.domains.iter().map(|(id, members)| (*id, members))
    }

    /// Each listen declaration, paired with the domain's members.
    ///
    /// A listen on a domain that was never declared yields nothing: failing
    /// closed means a typo costs silence rather than a leak.
    pub fn listeners(&self) -> impl Iterator<Item = (ConnectionId, &BTreeSet<ConnectionId>)> {
        self.listeners
            .iter()
            .filter_map(|(listener, domain)| Some((*listener, self.domains.get(domain)?)))
    }

    /// Each listen declaration as written, including ones naming a domain that
    /// does not exist.
    pub fn listen_declarations(&self) -> impl Iterator<Item = (ConnectionId, DomainId)> + '_ {
        self.listeners.iter().copied()
    }

    /// The edges declared one at a time.
    pub fn explicit_edges(&self) -> impl Iterator<Item = (ConnectionId, ConnectionId)> + '_ {
        self.edges.iter().copied()
    }

    /// Every directed edge implied by the declarations, as `(sender, receiver)`.
    ///
    /// A member never hears itself: the client plays back its own voice locally,
    /// and echoing it from the server is the classic doubled-voice bug. The
    /// explicit server loopback target is a separate mechanism and is not a
    /// route.
    ///
    /// This is the **definition** of the relation, and it materializes every
    /// pair - which is quadratic for a full-mesh domain. Nothing on a shard's
    /// turn calls it: [`compile`] walks the same structure without a tree, and
    /// the render's own checks walk it by distinct scope. It stays because it
    /// says plainly what the relation means, and a test pins [`compile`] to it.
    #[must_use]
    pub fn resolve(&self) -> BTreeSet<(ConnectionId, ConnectionId)> {
        let mut resolved = self.edges.clone();

        for members in self.domains.values() {
            for sender in members {
                for receiver in members {
                    if sender != receiver {
                        resolved.insert((*sender, *receiver));
                    }
                }
            }
        }

        for (listener, members) in self.listeners() {
            for sender in members {
                if *sender != listener {
                    resolved.insert((*sender, listener));
                }
            }
        }

        resolved
    }
}

/// The compiled table the voice plane reads.
///
/// Replaced whole, never mutated in place: the voice plane loads it without a
/// lock, and mutating it would put a lock on the audio path.
///
/// The transport half of the guide's `Delivery` - the live UDP address, the OCB2
/// state, the output queue - is deliberately absent. Binding sessions to live
/// transports is the voice plane's job (guide 9.7, build step 8); what a shard
/// owes it is *who may hear whom, and from which version*.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioRouting {
    receivers: BTreeMap<SessionId, Vec<SessionId>>,
    since: BTreeMap<SessionId, u64>,
}

impl AudioRouting {
    /// The sessions that may hear `sender`, or an empty slice.
    #[must_use]
    pub fn receivers(&self, sender: SessionId) -> &[SessionId] {
        self.receivers.get(&sender).map_or(&[], Vec::as_slice)
    }

    /// The shard version at which `session` became visible.
    ///
    /// The voice plane gates on this: a receiver whose cursor has not reached
    /// the sender's `since` does not get the packet, because it has not been
    /// told the sender exists yet and would discard the audio anyway.
    ///
    /// It is per participant rather than per pair, which is what keeps the table
    /// O(N) instead of O(N squared).
    #[must_use]
    pub fn since(&self, session: SessionId) -> Option<u64> {
        self.since.get(&session).copied()
    }

    /// Whether `receiver` may hear `sender` at all, ignoring the cursor gate.
    #[must_use]
    pub fn may_hear(&self, sender: SessionId, receiver: SessionId) -> bool {
        self.receivers(sender).contains(&receiver)
    }

    /// Every sender that has at least one receiver.
    pub fn senders(&self) -> impl Iterator<Item = SessionId> + '_ {
        self.receivers.keys().copied()
    }
}

/// Compile the declared relation into the table the voice plane reads.
///
/// `session_of` resolves a connection to the session it is rendered under.
/// Connections with no rendered user are dropped from the table: a sender nobody
/// can see is a sender whose audio the client would discard (guide 1.2), and a
/// receiver with no session has nothing to deliver to.
///
/// `since` carries the version each session first appeared at, threaded through
/// from the shard so it survives recompilation.
///
/// # Cost
///
/// The compiled table is quadratic in a domain's size **by construction**: a
/// full mesh of M members really does have M·(M-1) directed routes, and no
/// representation of an adjacency list avoids writing them down. What this
/// avoids is doing so through an ordered set: the members are resolved once per
/// domain and the receiver lists are appended to directly, so the cost is a
/// quadratic number of `Vec` pushes rather than of tree insertions. That is the
/// difference between microseconds and tens of milliseconds at 500 connections.
pub fn compile(
    relation: &AudioRelation,
    session_of: &BTreeMap<ConnectionId, SessionId>,
    since: &BTreeMap<SessionId, u64>,
) -> AudioRouting {
    let mut receivers: BTreeMap<SessionId, Vec<SessionId>> = BTreeMap::new();
    let resolve = |connections: &BTreeSet<ConnectionId>| -> Vec<SessionId> {
        connections
            .iter()
            .filter_map(|connection| session_of.get(connection).copied())
            .collect()
    };

    let mut listening_on: BTreeMap<DomainId, Vec<SessionId>> = BTreeMap::new();
    for (listener, domain) in relation.listen_declarations() {
        if let Some(session) = session_of.get(&listener) {
            listening_on.entry(domain).or_default().push(*session);
        }
    }

    for (domain, members) in relation.domains() {
        let sessions = resolve(members);
        let empty: Vec<SessionId> = Vec::new();
        let listening = listening_on.get(&domain).unwrap_or(&empty);

        for (index, sender) in sessions.iter().enumerate() {
            let list = receivers.entry(*sender).or_default();
            list.extend(
                sessions
                    .iter()
                    .enumerate()
                    .filter(|(other, _)| *other != index)
                    .map(|(_, receiver)| *receiver),
            );
            list.extend(listening.iter().filter(|listener| *listener != sender));
        }
    }

    for (sender, receiver) in relation.explicit_edges() {
        let (Some(sender), Some(receiver)) = (session_of.get(&sender), session_of.get(&receiver))
        else {
            continue;
        };
        receivers.entry(*sender).or_default().push(*receiver);
    }

    for list in receivers.values_mut() {
        // Sorted for determinism, deduplicated because a connection reachable
        // through both a domain and an explicit edge is still one receiver.
        list.sort_unstable();
        list.dedup();
    }
    receivers.retain(|_, list| !list.is_empty());

    AudioRouting {
        receivers,
        since: since.clone(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn sessions(pairs: &[(u64, u32)]) -> BTreeMap<ConnectionId, SessionId> {
        pairs
            .iter()
            .map(|(connection, session)| (ConnectionId(*connection), SessionId(*session)))
            .collect()
    }

    #[test]
    fn a_domain_is_symmetric_and_excludes_self() {
        let mut relation = AudioRelation::default();
        relation.domain(
            DomainId(1),
            &[ConnectionId(1), ConnectionId(2), ConnectionId(3)],
        );

        let edges = relation.resolve();
        assert!(edges.contains(&(ConnectionId(1), ConnectionId(2))));
        assert!(edges.contains(&(ConnectionId(2), ConnectionId(1))));
        assert!(
            !edges.contains(&(ConnectionId(1), ConnectionId(1))),
            "echoing a speaker back to itself is the doubled-voice bug"
        );
        assert_eq!(edges.len(), 6, "three members, every ordered pair but self");
    }

    #[test]
    fn a_listener_hears_without_being_heard() {
        let mut relation = AudioRelation::default();
        relation.domain(DomainId(1), &[ConnectionId(1), ConnectionId(2)]);
        relation.listen(ConnectionId(9), DomainId(1));

        let edges = relation.resolve();
        assert!(edges.contains(&(ConnectionId(1), ConnectionId(9))));
        assert!(edges.contains(&(ConnectionId(2), ConnectionId(9))));
        assert!(
            !edges.contains(&(ConnectionId(9), ConnectionId(1))),
            "a spectator must stay silent"
        );
    }

    #[test]
    fn listening_to_an_undeclared_domain_grants_nothing() {
        let mut relation = AudioRelation::default();
        relation.listen(ConnectionId(9), DomainId(404));

        assert!(
            relation.resolve().is_empty(),
            "a typo must cost silence, never a leak"
        );
    }

    #[test]
    fn compiling_drops_connections_with_no_rendered_user() {
        let mut relation = AudioRelation::default();
        relation.domain(DomainId(1), &[ConnectionId(1), ConnectionId(2)]);

        // Only connection 1 is rendered, so no edge survives: it has nobody
        // visible to talk to.
        let routing = compile(&relation, &sessions(&[(1, 100)]), &BTreeMap::new());
        assert!(routing.receivers(SessionId(100)).is_empty());
    }

    #[test]
    fn a_receiver_reachable_twice_is_listed_once() {
        let mut relation = AudioRelation::default();
        relation.domain(DomainId(1), &[ConnectionId(1), ConnectionId(2)]);
        relation.edge(ConnectionId(1), ConnectionId(2));

        let routing = compile(
            &relation,
            &sessions(&[(1, 100), (2, 200)]),
            &BTreeMap::new(),
        );
        assert_eq!(routing.receivers(SessionId(100)), &[SessionId(200)]);
    }

    #[test]
    fn compile_agrees_with_the_relations_definition() {
        // `compile` walks the structure directly for speed while `resolve`
        // spells out what the relation means. Nothing keeps them together
        // except this: a shape with overlapping domains, a listener, a shared
        // member and a stray edge, compiled both ways.
        let mut relation = AudioRelation::default();
        relation.domain(
            DomainId(1),
            &[ConnectionId(1), ConnectionId(2), ConnectionId(3)],
        );
        relation.domain(DomainId(2), &[ConnectionId(3), ConnectionId(4)]);
        relation.listen(ConnectionId(5), DomainId(1));
        relation.listen(ConnectionId(5), DomainId(2));
        relation.listen(ConnectionId(3), DomainId(2));
        relation.listen(ConnectionId(6), DomainId(404));
        relation.edge(ConnectionId(4), ConnectionId(1));
        relation.edge(ConnectionId(1), ConnectionId(2));

        let session_of = sessions(&[(1, 10), (2, 20), (3, 30), (4, 40), (5, 50), (6, 60)]);
        let compiled = compile(&relation, &session_of, &BTreeMap::new());

        let mut expected: BTreeMap<SessionId, Vec<SessionId>> = BTreeMap::new();
        for (sender, receiver) in relation.resolve() {
            let (Some(sender), Some(receiver)) =
                (session_of.get(&sender), session_of.get(&receiver))
            else {
                continue;
            };
            expected.entry(*sender).or_default().push(*receiver);
        }
        for list in expected.values_mut() {
            list.sort_unstable();
            list.dedup();
        }

        for (sender, receivers) in &expected {
            assert_eq!(
                compiled.receivers(*sender),
                receivers.as_slice(),
                "compile disagrees with resolve for sender {sender:?}"
            );
        }
        assert_eq!(
            compiled.senders().collect::<Vec<SessionId>>(),
            expected.keys().copied().collect::<Vec<SessionId>>(),
            "compile and resolve must agree on which senders exist at all"
        );
    }

    #[test]
    fn since_is_carried_per_participant() {
        let since = BTreeMap::from([(SessionId(100), 7), (SessionId(200), 9)]);
        let routing = compile(&AudioRelation::default(), &BTreeMap::new(), &since);

        assert_eq!(routing.since(SessionId(100)), Some(7));
        assert_eq!(routing.since(SessionId(200)), Some(9));
        assert_eq!(routing.since(SessionId(300)), None);
    }
}
