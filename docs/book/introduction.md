# Introduction

## Mumble

Mumble is a low-latency voice chat protocol, with free client implementations
for Windows, macOS, Linux, Android and iOS. Participants are placed in a tree of
channels held by the server, and audio is exchanged between participants that
share a channel. In the usual deployment, the tree comes from a configuration
file and an administrative interface: its shape is decided ahead of time, and
changes to it are the work of an operator or of a participant moving between
channels.

## Voxloom

Voxloom is a server implementation of that protocol, driven by code instead of
by configuration. Nothing about the channel tree, the participant list or the
audio topology is fixed in a file. All of it is produced from the state of an
external authority: a game server, a match, a dispatch system, a training
exercise, whatever system already holds the truth.

Connections come from unmodified official Mumble clients. No plugin, no protocol
extension and no client-side configuration are involved.

## A projection of authoritative state

Whenever the authoritative state changes, the affected part of the voice session
is recomputed and the difference is sent to each connection concerned. The voice
session is therefore a live projection of that state rather than a structure
maintained in parallel with it.

Stated as examples, with a game server as the authority:

- a player crossing into a cave is placed in the voice topology of that cave,
- the end of a round dissolves the team channels and restores the lobby,
- a role granted mid-game changes who is audible to whom,
- a player being spectated is moved out of the channel of the living.

None of these require a participant to select a channel, and none require an
operator. The move in the game and the move in the voice session are the same
event.

## Views

A *view* is what a single client holds: the channel tree presented to it, the
participants listed in that tree, and the speakers it is able to hear.

Views in Voxloom are per connection, and they are allowed to disagree. Two
clients connected to the same server, at the same moment, can hold entirely
different trees. Each client receives an ordinary Mumble session and has no way
of telling that anything unusual is happening.

## What a shared tree cannot express

With one tree shared by everyone, membership in a channel is the only vocabulary
available, and it answers the visibility question and the audio question at
once. Topologies that separate the two have no expression:

- two teams that must not see each other, sharing a lobby before the match,
- spectators who hear both sides without being audible to either,
- staff present in no channel until addressing one,
- proximity, where audibility is a function of distance rather than of position
  in a tree.

Approximating these with per-participant permissions and scripted channel moves
produces a model whose special cases grow with the number of roles.

## Ownership

The voice state belongs to the runtime: connections, sessions, encryption keys,
and the view each client holds. The authoritative state belongs to the
application and is never inspected by the runtime, which has no notion of a
player, a match or a team.

The interface between the two is a render function, supplied by the application
and called whenever the authoritative state changes. Its output is a shared
view, a set of scopes describing who observes what, and a directed audio
relation. The translation into Mumble messages is derived from that output
alone.

## Three mechanisms

Visibility and audibility are not the same question, and are answered by
separate mechanisms.

| mechanism | describes | used for |
|---|---|---|
| scope | a group | teams, spectators, staff, the common case |
| overlay | an individual exception | a private channel, a hidden participant |
| audio relation | who hears whom, one way or both | all audio |

The rule that selects between the first two: a scope describes a group, an
overlay describes an individual exception. A scope observed by a single
connection is an overlay under another name.

One constraint couples the mechanisms, imposed by the client rather than by the
design: audio from a session unknown to the client is discarded, so a receiver
has to see the speaker. A render whose audio relation violates that is refused
rather than partially applied.

## Cost

Rendering is done once per shard, not once per connection. A change produces a
delta, filtered independently for each connection. The cost of propagating a
change is therefore proportional to the size of the delta times the number of
connections, and is independent of the size of the world. That property holds
even when every connection holds a different view, which is the case Voxloom
exists for.

## Reading this book

The book is ordered by increasing commitment, and each part is a complete
reading on its own.

- **What Voxloom does** covers the capabilities and their limits, without types
  or code.
- **The model** covers scopes, overlays, audio relations, deltas and cost.
  Sufficient to evaluate whether a given topology is expressible.
- **Mumble compatibility** covers the implemented protocol surface and the
  method used to establish it.
- **Building an application** covers the render function, the shard, the
  connection router, and the configuration of a running server.

Generated API documentation is published separately and linked from the
reference section.

Voxloom is distributed under the AGPL-3.0-or-later license.
