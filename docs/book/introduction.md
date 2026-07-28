# Introduction

## Mumble

Mumble is a low-latency voice communication protocol. Free client
implementations are available for Windows, macOS, Linux, Android and iOS.

Participants are represented in a tree of channels maintained by the server.
Audio is normally exchanged between participants in the same channel.

In a conventional deployment, one channel tree is shared by all participants.
Its structure is defined through configuration or administrative actions.
Channel changes are initiated by participants, administrators or external
scripts.

## Mumble in games

Mumble is often used alongside multiplayer games, particularly for positional
audio.

Through Mumble Link, positional and contextual data may be transferred from a
game client to the local Mumble client. Audio positioning can then be derived
from information such as the current map, player position and orientation.

This integration remains local to each participant. No authoritative game state
is synchronized with the Mumble server through Mumble Link.

Authoritative state is often held by the game server instead. It may include:

- team membership,
- match phase,
- player role,
- alive or spectator state,
- location or region,
- permissions derived from game rules.

Voice behaviour based on this state usually requires custom channel management,
permissions or scripted moves. The resulting voice structure is maintained in
parallel with the game state.

## Mumble Server Runtime

Mumble Server Runtime is a Mumble-compatible server for application-controlled
voice sessions.

The channel structure, participant visibility and audio routing are derived
from authoritative state held by another system. That authority may be a game
server, a match controller, a dispatch system or a training platform.

No voice topology is fixed in a configuration file. The current voice session
is computed from the current application state.

Voice rules are defined in application code. Changes to authoritative state are
therefore reflected in the corresponding Mumble session without a separately
maintained channel model.

Connections are accepted from unmodified Mumble clients. No plugin, custom
client or protocol extension is required.

Mumble Link and Mumble Server Runtime address separate parts of game
integration. Positional metadata may still be provided through Mumble Link,
while server-controlled visibility, grouping and audio routing are derived
through the runtime.

## Application-controlled voice

A voice session consists of three elements:

- the channels visible to each connection,
- the participants visible to each connection,
- the directed audio relation between participants.

After a relevant change in authoritative state, the affected portion of the
voice session is recomputed. Only the resulting difference is propagated to the
affected connections.

The voice session is therefore a live projection of application state rather
than a second structure maintained independently.

For a game, the rendered voice state may include:

- placement in a cave-specific voice group after entry into a cave,
- removal of team channels at the end of a round,
- modified audibility after assignment of a role,
- removal from the living-player view after transition to spectator state,
- audibility derived from distance rather than channel membership.

No manual channel selection or administrative intervention is required. A
change in application state and the corresponding change in voice state form
one operation.

## Per-connection views

A _view_ is the portion of a Mumble session represented to one connection. It
contains the visible channel tree, the participants represented within that
tree and the speakers whose audio may be received.

Views are computed independently for each connection and may differ at the same
instant. Different channel trees may therefore be sent to two clients connected
to the same server.

For example, only one team may be visible to a player, while both teams remain
visible to a spectator. A separate structure may be visible to staff members.

Each result remains an ordinary Mumble session from the client perspective.

## Visibility and audibility

_Visibility_ is the set of channels and participants represented to a client.

_Audibility_ is the directed relation defining which participants may receive
audio from which speakers.

These concerns are represented separately. Visibility does not imply
audibility, and audibility may be one-way.

The following topologies can therefore be represented:

- teams hidden from one another while sharing a pre-match lobby,
- spectators receiving audio from both teams without being audible,
- staff remaining outside participant views until addressing a group,
- proximity voice derived from distance,
- private communication visible only to selected participants.

A single shared channel tree cannot represent these cases directly. Channel
membership is the only available grouping mechanism, so visibility and
audibility remain coupled. Approximation through permissions and scripted
channel moves requires additional special cases for each role.

## Responsibility boundary

Application state remains outside the runtime. Domain concepts such as players,
matches, teams, roles and locations are not inspected.

Runtime ownership is limited to Mumble-specific state:

- network connections,
- protocol sessions,
- encryption state,
- client views,
- audio routing.

Two directions cross that boundary.

Downward, state. A render function is supplied by the application and evaluated
after a relevant change in authoritative state. Its result consists of the
desired views and directed audio relations. Mumble protocol messages are derived
exclusively from that result.

Upward, events. Client-initiated actions are reported to the application:
connection and disconnection, a request to enter a visible channel, a request to
self-mute or self-deafen, and the invocation of a menu entry defined by the
application itself.

Such events are requests rather than facts. No voice state is modified by their
delivery. Authoritative state is updated by the application when a request is
accepted, and the consequence becomes visible through the next render. A refusal
consists of rendering nothing new, optionally accompanied by a message returned
to the connection.

At a high level:

```text
                       render
  authoritative state ───────▶ voice state ───────▶ Mumble sessions
          ▲                                                │
          └────────────────────────────────────────────────┘
                               events
```

## Reading this book

The documentation is divided into the following parts:

- **Boundaries** states what the runtime does not do, and which values are fixed
  rather than configurable.
- **The model** defines views, scopes, overlays, audio relations, deltas and
  propagation cost. No types or code appear there.

Generated API documentation is published separately. See
[API documentation](reference/api.md).
