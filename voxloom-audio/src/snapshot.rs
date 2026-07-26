//! The routing snapshot: what the packet path reads, and how it is built.
//!
//! Spec 15.3 fixes the shape (`generation`, `routes`, `senders`) and leaves the
//! representation open, listing compact recipient lists as one option. That is
//! what this uses: one contiguous buffer holding, for each sender, its own
//! session followed by its recipients. A lookup is therefore a hash probe plus a
//! slice borrow, with no allocation and no branch per recipient.
//!
//! Keeping the sender itself in the buffer is not decoration: it makes the
//! server-loopback target an ordinary one-element route instead of a special
//! case carved out of the packet path.

use std::collections::HashMap;
use std::sync::Arc;

use crate::policy::AudioTarget;

/// A voice session identifier, as carried by `Audio.sender_session` on the wire.
///
/// A newtype rather than a bare `u32` because the packet path juggles several
/// unrelated 32-bit identifiers (targets, contexts, sessions) and confusing two
/// of them would route voice to the wrong listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(u32);

impl SessionId {
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

/// The routing partition a session belongs to.
///
/// One domain exists today and the compiled policy is trivial, but the type is
/// present in every snapshot key from the first line of the router. Partitioning
/// has to be structural before it is useful: retrofitting it into the route
/// representation, the metadata and every call site later is a rewrite, not a
/// patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RoutingDomainId(u32);

impl RoutingDomainId {
    /// The single domain every session lands in until partitioning becomes real.
    pub const DEFAULT: Self = Self(0);

    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

/// One participant handed to [`compile`]: a session and the domain it routes in.
///
/// The compiler takes participants rather than reading canonical state, because
/// the hot path must not own a route, even a transitive one, into live state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Participant {
    pub session: SessionId,
    pub domain: RoutingDomainId,
}

impl Participant {
    pub const fn new(session: SessionId, domain: RoutingDomainId) -> Self {
        Self { session, domain }
    }
}

/// Where each sender's recipients live inside the flat buffer.
#[derive(Debug, Clone, Copy)]
struct Entry {
    /// Index of the sender's own session; its recipients follow immediately.
    start: usize,
    /// How many recipients follow.
    recipients: usize,
}

/// Precompiled recipient lists, one contiguous run per sender (spec 15.3).
#[derive(Debug, Default)]
pub struct RouteMatrix {
    entries: Vec<SessionId>,
    index: HashMap<SessionId, Entry>,
}

impl RouteMatrix {
    /// The sessions `sender` reaches with normal speech. Empty for an unknown
    /// sender, which is the fail-closed answer: no route means no delivery.
    pub fn recipients_of(&self, sender: SessionId) -> &[SessionId] {
        match self.index.get(&sender) {
            // Checked slicing rather than `[a..b]`: the indices are built here
            // and cannot be out of range, but a panic in the packet path would
            // take the whole voice plane down with it.
            Some(entry) => self
                .entries
                .get(entry.start + 1..entry.start + 1 + entry.recipients)
                .unwrap_or(&[]),
            None => &[],
        }
    }

    /// The one-element slice holding `sender` itself, used by the loopback
    /// target. Empty for an unknown sender.
    pub fn just_sender(&self, sender: SessionId) -> &[SessionId] {
        match self.index.get(&sender) {
            Some(entry) => self
                .entries
                .get(entry.start..entry.start + 1)
                .unwrap_or(&[]),
            None => &[],
        }
    }

    /// How many senders the matrix knows.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
}

/// Per-sender facts the directional policy needs (spec 15.3).
#[derive(Debug, Default)]
pub struct SenderMetadata {
    domains: HashMap<SessionId, RoutingDomainId>,
}

impl SenderMetadata {
    /// The domain a session routes in, or `None` if the snapshot predates it.
    pub fn domain_of(&self, session: SessionId) -> Option<RoutingDomainId> {
        self.domains.get(&session).copied()
    }
}

/// The immutable routing decision table the packet path consults (spec 15.3).
///
/// Published by atomic swap (spec 23.2): readers hold an `Arc` to a generation
/// that never changes under them, so consulting one needs no synchronisation at
/// all. The inner `Arc`s let a new generation share the parts that did not move.
#[derive(Debug, Clone)]
pub struct AudioRoutingSnapshot {
    generation: u64,
    routes: Arc<RouteMatrix>,
    senders: Arc<SenderMetadata>,
}

impl AudioRoutingSnapshot {
    /// Which generation this is. Monotonic across publications, so an observer
    /// can tell whether it read a stale table without comparing contents.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn routes(&self) -> &RouteMatrix {
        &self.routes
    }

    pub fn senders(&self) -> &SenderMetadata {
        &self.senders
    }

    /// The candidate recipients for one packet: precompiled, borrowed, never
    /// rebuilt. The caller still asks [`crate::may_receive`] about each one,
    /// which is where the packet-dependent part of the policy lives.
    pub fn receivers(&self, sender: SessionId, target: AudioTarget) -> &[SessionId] {
        match target {
            AudioTarget::Normal => self.routes.recipients_of(sender),
            AudioTarget::ServerLoopback => self.routes.just_sender(sender),
            // Registered targets are set up with a `VoiceTarget` control
            // message, which is not accepted yet: no route, so nothing is
            // delivered (fail closed) rather than falling back to normal speech.
            AudioTarget::Registered(_) => &[],
        }
    }

    /// The domain `session` routes in, or `None` if this snapshot does not know
    /// it (it connected after the snapshot was compiled).
    pub fn domain_of(&self, session: SessionId) -> Option<RoutingDomainId> {
        self.senders.domain_of(session)
    }
}

/// Compile the participants into a routing snapshot (the cold path of ADR-005).
///
/// Phase 4's policy is deliberately trivial: everyone hears everyone else in
/// their own domain. The cost is quadratic in the participant count, which is
/// the honest shape for "every pair is a route" and is affordable because this
/// runs on presence changes, not per packet. A real policy (proximity,
/// permissions) replaces this body without changing anything the packet path
/// sees.
///
/// Duplicate sessions are collapsed and the output is ordered, so the same
/// participants always compile to the same table regardless of the order they
/// arrive in. Determinism is what makes the snapshot property-testable.
///
/// A session declared in two different domains is contradictory input. Routing
/// it either way would be a guess, and a wrong guess is voice crossing a
/// partition, so such a session is dropped from the table entirely: it neither
/// hears nor is heard until the caller resolves the contradiction. Collapsing it
/// to whichever declaration happened to sort first would make partition
/// isolation depend on input ordering.
pub fn compile(participants: &[Participant], generation: u64) -> AudioRoutingSnapshot {
    let mut ordered = participants.to_vec();
    ordered.sort_by(|left, right| {
        left.session
            .cmp(&right.session)
            .then(left.domain.cmp(&right.domain))
    });

    let mut sorted: Vec<Participant> = Vec::with_capacity(ordered.len());
    for run in ordered.chunk_by(|left, right| left.session == right.session) {
        let Some(first) = run.first() else {
            continue;
        };
        if run
            .iter()
            .all(|participant| participant.domain == first.domain)
        {
            sorted.push(*first);
        }
    }

    let mut entries: Vec<SessionId> = Vec::new();
    let mut index: HashMap<SessionId, Entry> = HashMap::with_capacity(sorted.len());
    let mut domains: HashMap<SessionId, RoutingDomainId> = HashMap::with_capacity(sorted.len());

    for participant in &sorted {
        let start = entries.len();
        entries.push(participant.session);
        for other in &sorted {
            if other.session != participant.session && other.domain == participant.domain {
                entries.push(other.session);
            }
        }
        let recipients = entries.len() - start - 1;
        index.insert(participant.session, Entry { start, recipients });
        domains.insert(participant.session, participant.domain);
    }

    AudioRoutingSnapshot {
        generation,
        routes: Arc::new(RouteMatrix { entries, index }),
        senders: Arc::new(SenderMetadata { domains }),
    }
}
