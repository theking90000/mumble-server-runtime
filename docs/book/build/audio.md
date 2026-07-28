# Declaring audio

Audibility is declared inside `render`, alongside the tree, through three calls:

```rust
out.audio_domain(RED_VOICE, &red_members);  // symmetric group
out.audio_listen(spectator, RED_VOICE);     // one way
out.audio_edge(admin, member);              // one directed pair
```

What they build is a directed relation over connections, independent of the
channel tree and of the scope tree. [Audio as a relation](../model/audio.md)
defines it. This page covers declaring one.

## The relation is rebuilt every render

Silence is the starting point of every render, not a state that persists between
them. Nothing carries over, so a route is withdrawn by not declaring it again.

A `DomainId` therefore needs no stability across renders, unlike a `ChannelKey`.
It is a label within one render, used to connect a group to its listeners, and
nothing compares it against the previous turn.

```rust
const RED_VOICE: DomainId = DomainId(1);
```

Constants are still the readable choice, but a domain identifier computed from a
match number would be equally correct.

## Choosing among the three

| intent | declaration |
| --- | --- |
| a group whose members all hear each other | one `audio_domain` |
| an observer hearing a group without being heard | `audio_listen` |
| one person addressing another | `audio_edge` |
| anything the first two do not express | `audio_edge` |

A domain expresses a full mesh in one declaration. The pair loop is never
written by the application, which is the difference between describing a group
and materialising it.

A listen naming a domain that was never declared yields nothing. A typo costs
silence rather than disclosure.

## Assembling a domain across loops

Repeated calls under one identifier accumulate members rather than replacing
them, so a domain may be built wherever the members happen to be iterated:

```rust
for (connection, role) in &self.roles {
    if let Role::Player(side) = role {
        out.audio_domain(voice_of(*side), &[*connection]);
    }
}
```

## Mute and deafen are not audio declarations

The mute, suppress and deafen flags are rendered on the participant, through
`user_flags`, and compiled into the routing table from there:

```rust
let user = out.user(red, Occupant::Connection(who), &name, Narrow::Same);
out.user_flags(user, flags);
```

Declaring a flag and then omitting the corresponding audio declaration would say
the same thing twice, and the two statements would eventually disagree. A muted
participant stays in their domain.

## An edge and its overlay travel together

Every declared route requires the receiver to see the sender. A render placing
an edge onto a receiver who cannot is refused whole.

In practice this pairs one audio declaration with one visibility declaration:

```rust
for member in self.members_of(side) {
    out.private(member, |private| {
        private.user_in(base, Occupant::Connection(admin), &label);
    });
    out.audio_edge(admin, member);
}
```

Writing the edge without the overlay entry is the most common way to build a
render the runtime refuses. The two are one decision. See
[Seeing the speaker](../model/coupling.md).

## Proximity

Distance-based audibility is the one shape where the number of edges grows with
the population, and that is inherent rather than a limitation of the model:

```rust
for (speaker, position) in self.positions() {
    for listener in self.within(position, HEARING_RANGE) {
        out.audio_edge(speaker, listener);
    }
}
```

Everyone involved has to be mutually visible for those edges to mean anything,
so proximity voice is usually rendered as one shared channel holding every
participant in range of anyone, with the relation doing the selection. The
alternative, a scope per neighbourhood, exhausts the four observed scopes as
soon as neighbourhoods overlap.

## What not to do

Do not enumerate pairs where a domain applies. The relation has a `resolve`
operation materialising every pair, and no shard turn calls it. It exists to
state plainly what the declarations mean, and a test pins the compiled table to
it. Turning it into a code path reintroduces the quadratic shape the model
exists to avoid.

Do not attempt to grant a right to speak separately. There is none. Speaking
somewhere implies being visible there.
