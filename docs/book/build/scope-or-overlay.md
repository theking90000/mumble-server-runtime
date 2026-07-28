# Scope or overlay

A scope describes a group, an overlay describes an individual exception. That is
the rule, stated in [The shared view and scopes](../model/scopes.md) and
[Overlays](../model/overlays.md). This page is about applying it.

## The two mechanisms answer different questions

The name of this page is slightly misleading, and the confusion is worth
clearing first. Scopes and overlays are not two ways of doing one thing.

A scope answers **what does this connection see**. That answer is always a
scope, returned from `observation`, and there is no alternative mechanism for
it.

An overlay answers **who else sees this element**, for an element that would
otherwise be shared. Its alternative is the shared view, not a scope.

The demonstration application makes the split visible. Staff observe three
scopes, the same set spectators observe, so what they *see* is expressed in
scopes. Their own presence is rendered into no shared channel at all, only into
overlays. One connection, both mechanisms, each for its own question.

```rust
fn observation(&mut self, connection: ConnectionId) -> ScopeSet {
    match self.roles.get(&connection) {
        Some(Role::Player(side)) => ScopeSet::new(&[side_scope(*side)]),
        Some(Role::Spectator | Role::Admin { .. }) => watcher_view(),
        None => ScopeSet::new(&[Scope::ROOT]),
    }
    .unwrap_or(ScopeSet::NONE)
}
```

## Placing an element

Once observation is settled, each element rendered is either shared, and
therefore seen by everyone whose observation reaches its scope, or private to
the connections whose overlay carries it.

| situation | placement |
| --- | --- |
| team channels and their members | shared, at the team's scope |
| a spectator watching both teams | shared, at a scope of its own |
| a vanished administrator | overlay, their own |
| an administrator addressing one team | one overlay entry per member of that team |
| a private channel one person holds | overlay |
| a per-connection button | overlay, as a context action |
| proximity voice | neither: an audio relation, with no structural change |

The last row matters more than it looks. Audibility is a third mechanism, not a
consequence of the first two, so a distinction that is purely about who hears
whom needs no scope and no overlay. Reaching for one is how special cases start.
See [Audio as a relation](../model/audio.md).

## When a scope is an overlay in disguise

A scope with one observer is an overlay in disguise. A role is a group by
nature even while it holds a single member: player, host, spectator, staff. The
test is not the current headcount but whether a second member would be a change
of data or a change of model.

Two bounds turn that judgement into a hard limit: a scope is at most four
segments deep, and a connection observes at most four scopes. Running out is a
modelling signal rather than a configuration problem. A design needing a fifth
observed scope is usually describing individuals with a group mechanism.

## Cost

| mechanism | cost |
| --- | --- |
| shared view and scopes | `O(W)` once, plus `O(\|D\|)` per connection |
| overlay | `O(\|overlay\|)` per connection, every turn |
| audio relation | `O(N + edges)` |

A scope is rendered once whatever the number of observers. An overlay is
recomputed for its connection at every turn and never journalled, so nothing
amortises it.

That asymmetry is the whole argument. Overlays stay cheap while they stay
exceptional. An overlay whose size grows with the population is the quadratic
shape the model exists to avoid, arriving through the one door left open. See
[Cost](../model/cost.md).

## A worked decision

A game wants an administrator who is invisible, hears everything, and can
address one team at a time.

**Observation.** The administrator has to see both teams to address either, so
their observation is the watcher set. A scope, like every observation.

**Presence.** Invisible means absent from the shared view rather than present
and suppressed. Nothing is rendered for them in any shared channel. Their own
client still needs to find itself when the handshake names its session, so one
overlay carries their presence to themselves alone.

**Hearing.** One-way listening on both team domains. No structural change.

**Addressing a team.** Two declarations that cannot be separated: an overlay
entry giving each member of that team sight of the administrator, and a directed
audio edge to each. Declaring the edge alone builds a render the runtime
refuses, because a receiver who cannot see the sender is a receiver whose client
discards the audio. See [Seeing the speaker](../model/coupling.md).
