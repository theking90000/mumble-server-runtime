# Running the arena

The arena is the demonstration application shipped with the runtime. It is a
lobby and a match running as two shards, and it is the only crate in the
repository that names both the runtime and a concrete business. Everything below
it is generic, everything inside it is a game.

Each of the three visibility mechanisms is used there for exactly one thing, so
that reading the code answers which mechanism fits which problem:

- teams as scopes, so a red player cannot see that blue exists,
- a vanished admin as a private overlay,
- spectators as one-way listeners, hearing both teams and heard by neither.

## Starting it

```console
$ cargo run -p mumble-server-runtime-arena
```

Two optional positional arguments follow, in order:

| argument | default | meaning |
| --- | --- | --- |
| bind address | `0.0.0.0:64738` | where the gateway listens |
| admission ceiling | `100` | how many connections are accepted |

```console
$ cargo run -p mumble-server-runtime-arena -- 127.0.0.1:64738
```

The address is worth overriding on a machine already running a Mumble server,
which holds port 64738.

A self-signed certificate is generated at every start. A real deployment
supplies a persistent one instead, otherwise each restart looks like a new
server to clients, which key their local preferences on the certificate hash.

## Connecting

Point a Mumble client at `127.0.0.1`, port `64738`. Use the address rather than
`localhost`, which resolves to IPv6 first and finds nothing listening.

A name is required: a connection arriving without one is rejected by the router
with a reason. Typing `overwatch` into the password field joins as staff.

That credential is a placeholder for a real token flow, kept deliberately
obvious. What matters is its shape. An opaque credential is carried by the
gateway, and what it buys is decided by the application alone.

## The lobby

Every arrival is attached to the lobby. It is one flat room where everyone sees
and hears everyone, and it uses no scopes at all: there is one group, and
reaching for a mechanism because it exists would be the mistake the crate is
meant to illustrate.

Four channels are offered, and double-clicking one states an intent rather than
performing a move:

| channel | effect |
| --- | --- |
| Red Team | chooses the red side |
| Blue Team | chooses the blue side |
| Spectators | chooses to watch |
| `> Enter the Arena` | requests migration to the arena shard |

The same door is also offered as a context menu entry, per connection rather
than shared. A connection that has chosen no side is told so when the entry is
invoked, which a channel cannot do.

The root channel name carries a counter incremented once per second from outside
the shard. It demonstrates the path business work takes to reach a render: the
producer sends an update on a channel it owns, wakes the shard, and the pending
updates are drained during the next render.

## The arena

Five channels, three of which are at distinct scopes:

| channel | scope | visible to |
| --- | --- | --- |
| Red Base | `/1` | red players, spectators, staff |
| Blue Base | `/2` | blue players, spectators, staff |
| Observation Deck | `/3` | spectators, staff |
| Neutral Ground | root | everyone in the arena |
| `< Back to the Lobby` | root | everyone in the arena |

```text
  scope tree        observed by            sees

  /                 unplaced arrival       /
  |
  +- /1             red player             /, /1
  |
  +- /2             blue player            /, /2
  |
  +- /3             spectator, staff       /, /1, /2, /3
```

Red Base is not hidden from a blue player by a filter that could be given the
wrong side of. It sits at a scope their observation is not comparable to, so it
is absent from every delta they will ever receive. One render produces both
teams' views.

Neutral Ground and the door back to the lobby stay at the root scope, which is
what makes them the only two things every occupant can act on.

## The three mechanisms, side by side

**Teams are scopes.** A player observes one team scope. Membership of the other
team is not filtered late, it is never planned.

**Staff are an overlay.** Staff occupy no shared channel. Their only presence is
in overlays: their own, which is what lets the client find itself when the
handshake names its session, and, when a team is addressed, that team's. The
vanish is an absence rather than a flag, so there is nothing downstream to
filter out.

**Hearing is a relation.** Spectators sit at their own scope and listen to both
team domains. One-way is what listening is, rather than a rule someone has to
remember. Staff listen to everything and are heard by nobody until an explicit
edge is opened.

Opening that edge and adding the overlay entry are one decision, not a
convention. A render placing an audio edge onto a receiver who cannot see the
sender is refused, because the Mumble client discards audio from a session it
does not know. See [Seeing the speaker](../model/coupling.md).

## What to try

Two clients are enough for the scope mechanism. Pick different teams in the
lobby, enter the arena, and neither base is visible to the other side.

A third client connected with the staff credential disappears from every view
but its own, hears both teams, and becomes visible and audible to exactly one
team the moment that team's base is double-clicked.

Double-clicking `> Enter the Arena` migrates a connection from the lobby shard
to the arena shard. The TCP connection, the session and every local client
setting attached to it are preserved. The view is transitioned onto what the
client already holds rather than rebuilt, so no reconnection occurs.

## Where the code is

| file | contents |
| --- | --- |
| `tools/mumble-server-runtime-arena/src/main.rs` | the composition: gateway, both shards, the update task |
| `.../src/router.rs` | where an arrival goes and what the credential buys |
| `.../src/lobby.rs` | the flat room, the intents, the migration request |
| `.../src/arena.rs` | scopes, overlays and the audio relation |
| `.../src/directory.rs` | application state shared by both shards |
