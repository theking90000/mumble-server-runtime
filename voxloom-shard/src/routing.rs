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
use crate::view::UserFlags;

/// Who may not speak, and who may not hear, as the rendered view says it.
///
/// The audio plane is the only place these flags mean anything: everywhere else
/// they are an icon. Compiling them into the table rather than testing them per
/// packet keeps the hot path free of the question, and makes a mute take effect
/// on the very turn that announces it - the table is published before any view.
///
/// A flavor owns the flags, as it owns everything else it renders. What it does
/// **not** own is whether a user it renders as muted can still be heard: a client
/// showing a crossed-out microphone next to someone whose voice comes through is
/// a lie the runtime would be telling on the flavor's behalf.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Silence {
    muted: BTreeSet<SessionId>,
    deafened: BTreeSet<SessionId>,
}

impl Silence {
    /// Record what one rendered user's flags mean for the audio plane.
    ///
    /// The two predicates are the reference server's own, field for field.
    ///
    /// REF: references/mumble/src/murmur/Server.cpp : `processMsg` drops the
    ///   packet before anything else when the speaker is
    ///   `bMute || bSuppress || bSelfMute`.
    /// REF: references/mumble/src/murmur/AudioReceiverBuffer.cpp : `addReceiver`
    ///   refuses a receiver that is `bDeaf || bSelfDeaf`.
    pub fn record(&mut self, session: SessionId, flags: UserFlags) {
        if flags.self_mute || flags.mute || flags.suppress {
            self.muted.insert(session);
        }
        if flags.self_deaf || flags.deaf {
            self.deafened.insert(session);
        }
    }

    /// Whether this session's voice may leave the server at all.
    #[must_use]
    pub fn may_speak(&self, session: SessionId) -> bool {
        !self.muted.contains(&session)
    }

    /// Whether this session may be given anyone's voice.
    #[must_use]
    pub fn may_hear(&self, session: SessionId) -> bool {
        !self.deafened.contains(&session)
    }
}

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
    /// Carried through so the one delivery that is **not** a route - the server
    /// loopback a client asks for explicitly - can ask the same question.
    silence: Silence,
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

    /// Whether this session's voice may leave the server at all.
    ///
    /// Every route already answers it - a muted sender has no receivers - so
    /// this exists for the one delivery that goes through no route: the server
    /// loopback. A muted microphone that still echoes back would tell its owner
    /// the line is open when nobody else can hear a thing.
    #[must_use]
    pub fn may_speak(&self, sender: SessionId) -> bool {
        self.silence.may_speak(sender)
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
/// `silence` removes muted senders and deafened receivers before any route is
/// written down, rather than after: a muted speaker in a domain of fifty costs
/// nothing at all here, where filtering the finished table would cost the fifty
/// routes it should never have had.
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
    silence: &Silence,
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
        if let Some(session) = session_of.get(&listener).filter(|s| silence.may_hear(**s)) {
            listening_on.entry(domain).or_default().push(*session);
        }
    }

    for (domain, members) in relation.domains() {
        let sessions = resolve(members);
        // Resolved once per domain rather than per pair: deafening one member of
        // a domain of M costs one filtered pass, not M tests inside the loop
        // that writes M squared routes.
        let audience: Vec<SessionId> = sessions
            .iter()
            .copied()
            .filter(|session| silence.may_hear(*session))
            .collect();
        let empty: Vec<SessionId> = Vec::new();
        let listening = listening_on.get(&domain).unwrap_or(&empty);

        for sender in sessions.iter().filter(|s| silence.may_speak(**s)) {
            let list = receivers.entry(*sender).or_default();
            // Compared by value rather than by position, because `audience` is
            // no longer index-aligned with `sessions`. Two connections never
            // share a session, so the two tests are the same test.
            list.extend(audience.iter().filter(|receiver| *receiver != sender));
            list.extend(listening.iter().filter(|listener| *listener != sender));
        }
    }

    for (sender, receiver) in relation.explicit_edges() {
        let (Some(sender), Some(receiver)) = (session_of.get(&sender), session_of.get(&receiver))
        else {
            continue;
        };
        if !silence.may_speak(*sender) || !silence.may_hear(*receiver) {
            continue;
        }
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
        silence: silence.clone(),
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
        let routing = compile(
            &relation,
            &sessions(&[(1, 100)]),
            &BTreeMap::new(),
            &Silence::default(),
        );
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
            &Silence::default(),
        );
        assert_eq!(routing.receivers(SessionId(100)), &[SessionId(200)]);
    }

    /// A `Silence` built the way a shard builds it: from rendered flags.
    fn silenced(flags: &[(u32, UserFlags)]) -> Silence {
        let mut silence = Silence::default();
        for (session, flags) in flags {
            silence.record(SessionId(*session), *flags);
        }
        silence
    }

    fn muted() -> UserFlags {
        UserFlags {
            self_mute: true,
            ..UserFlags::default()
        }
    }

    fn deafened() -> UserFlags {
        UserFlags {
            self_deaf: true,
            ..UserFlags::default()
        }
    }

    #[test]
    fn a_muted_speaker_has_no_receivers_at_all() {
        let mut relation = AudioRelation::default();
        relation.domain(
            DomainId(1),
            &[ConnectionId(1), ConnectionId(2), ConnectionId(3)],
        );
        relation.edge(ConnectionId(1), ConnectionId(4));
        relation.listen(ConnectionId(4), DomainId(1));

        let session_of = sessions(&[(1, 10), (2, 20), (3, 30), (4, 40)]);
        let routing = compile(
            &relation,
            &session_of,
            &BTreeMap::new(),
            &silenced(&[(10, muted())]),
        );

        assert!(
            routing.receivers(SessionId(10)).is_empty(),
            "a muted microphone must have no line to anyone, by any primitive"
        );
        assert!(!routing.may_speak(SessionId(10)));
        assert_eq!(
            routing.receivers(SessionId(20)),
            &[SessionId(10), SessionId(30), SessionId(40)],
            "muting silences a microphone, not an ear: the muted one is still a \
             receiver, and the others keep every line they had"
        );
    }

    #[test]
    fn a_deafened_receiver_appears_in_nobody_s_list() {
        let mut relation = AudioRelation::default();
        relation.domain(DomainId(1), &[ConnectionId(1), ConnectionId(2)]);
        relation.edge(ConnectionId(3), ConnectionId(2));
        relation.listen(ConnectionId(2), DomainId(1));

        let session_of = sessions(&[(1, 10), (2, 20), (3, 30)]);
        let routing = compile(
            &relation,
            &session_of,
            &BTreeMap::new(),
            &silenced(&[(20, deafened())]),
        );

        for sender in [SessionId(10), SessionId(30)] {
            assert!(
                !routing.receivers(sender).contains(&SessionId(20)),
                "a deafened session must not be a receiver of {sender:?}"
            );
        }
        assert_eq!(
            routing.receivers(SessionId(20)),
            &[SessionId(10)],
            "deafening silences the ear, not the microphone"
        );
        assert!(routing.may_speak(SessionId(20)));
    }

    #[test]
    fn the_server_flags_silence_exactly_as_their_self_counterparts_do() {
        // `mute` and `suppress` are the moderation equivalents of `self_mute`,
        // and `deaf` of `self_deaf`. Rendering one of them while the voice still
        // flows would put an icon on a lie.
        let server_muted = UserFlags {
            mute: true,
            ..UserFlags::default()
        };
        let suppressed = UserFlags {
            suppress: true,
            ..UserFlags::default()
        };
        let server_deafened = UserFlags {
            deaf: true,
            ..UserFlags::default()
        };

        let silence = silenced(&[(10, server_muted), (20, suppressed), (30, server_deafened)]);
        assert!(!silence.may_speak(SessionId(10)));
        assert!(!silence.may_speak(SessionId(20)));
        assert!(!silence.may_hear(SessionId(30)));
        assert!(
            silence.may_speak(SessionId(30)),
            "server-deafening is not server-muting"
        );
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
        let compiled = compile(
            &relation,
            &session_of,
            &BTreeMap::new(),
            &Silence::default(),
        );

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
        let routing = compile(
            &AudioRelation::default(),
            &BTreeMap::new(),
            &since,
            &Silence::default(),
        );

        assert_eq!(routing.since(SessionId(100)), Some(7));
        assert_eq!(routing.since(SessionId(200)), Some(9));
        assert_eq!(routing.since(SessionId(300)), None);
    }
}
