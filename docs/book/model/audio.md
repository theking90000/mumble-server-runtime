# Audio as a relation

Audio routing is a _directed relation over connections_: for each ordered pair,
whether the second may receive the first's voice. Declared outright, and
independent of both the channel tree and the scope tree.

The independence is deliberate. "Spectators hear a team without being heard" is
not a position in a tree, and forcing audibility to follow a visibility structure
is what makes such models accumulate special cases.

## Silence by default

Nothing is implicit. With no declaration, no audio is delivered. Sharing a
channel grants nothing, and sharing a scope grants nothing.

## Three primitives

**Domain.** A named group whose members all hear each other. Symmetric. A member
never receives its own voice back, since the client already plays it locally and
echoing it from the server is the classic doubled-voice defect. Members declared
repeatedly under one identifier are accumulated rather than replaced, so a domain
may be assembled across several loops.

**Listen.** One connection hears a domain without being heard by it. One way.

**Edge.** A single directed pair, for whatever the other two do not express.

Cost follows the shape: quadratic in a domain's size, linear in a listened
domain's size, constant for an edge.

```text
  audio_domain(RED, [alice, bob])   alice ◀───▶ bob

  audio_listen(carol, RED)          alice ────▶ carol
                                    bob   ────▶ carol

  audio_edge(dave, alice)           dave  ────▶ alice
```

Nothing points away from carol, so nobody hears her. Nothing points at dave, so
he hears nobody.

## Failing closed

A listen naming a domain that was never declared yields nothing. A typo costs
silence rather than disclosure.

## Mute and deafen

The mute, suppress and deafen flags rendered on a participant are compiled into
the routing table rather than tested on each packet. A mute therefore takes
effect on the very turn that announces it, since the table is published before
any view.

Compiling them in is not only an optimisation. A client showing a crossed-out
microphone next to someone whose voice still comes through would be a
contradiction produced by the runtime on the application's behalf.

## Seeing the speaker

One constraint crosses over from the protocol: a receiver must see the speaker.

Audio whose sender session is unknown is discarded by the Mumble client, so an
edge landing on a receiver that cannot see the sender would deliver nothing. The
condition is checked on the render's output, and a render violating it is refused
whole.

A corollary worth stating: there is no separate right to speak. Speaking
somewhere implies being visible there. See [Seeing the speaker](coupling.md).

## The compiled table

The declared relation is compiled once per turn into a table of receivers per
speaker, read by the voice plane without a lock and replaced whole rather than
mutated. No shard is consulted when a voice packet arrives.

Each speaker in the table carries the version at which it became visible. A
receiver whose position in the history has not reached that version is given
nothing, which is what keeps a newly granted route inert until the receiver has
been told the speaker exists. Revocation needs no such gate: the table is
published before any view is pushed, so cutting a route early is always safe.
