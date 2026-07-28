# Vocabulary

Terms used throughout this book, ordered so that each definition draws only on
terms already defined.

## Shard

A unit of rendering, ownership and scheduling. One shard holds one region of
application state, produces one shared view, and keeps one history of changes.
A connection is attached to exactly one shard at a time.

## Connection

One client socket, with its own encryption state and output queue. Identified by
a `ConnectionId`, which is the identifier application code works in.

## Occupant

Whoever fills a rendered participant slot. Either a connection, or a synthetic
participant with no socket: a non-player character, a bot, someone present in the
application but not in Mumble. The occupant is the participant's identity, so no
separate key is needed.

## Session identifier, channel identifier

The numbers carried on the wire. Allocated once and never reused, because local
client preferences are keyed on them: nickname overrides, per-participant volume,
channel filter mode. Reuse would apply one participant's settings to another.

## Channel key

A stable identity for a channel, chosen by the application and held across
renders. Distinct from the channel's name, which is an ordinary field. A channel
whose identity came from its name would be destroyed and recreated every time the
name changed.

## Shared view

Every channel and participant a shard renders, each fact held once regardless of
how many connections end up seeing it. Holding facts once instead of copies is
what keeps the cost linear.

## Scope

A position in a tree, carried by every rendered element and observed by every
connection. The mechanism that makes one shared view yield different results per
connection without duplicating it. See [The shared view and scopes](scopes.md).

## Overlay

The elements visible to exactly one connection. Reserved for individual
exceptions, where a scope describes a group. See [Overlays](overlays.md).

## View

What one connection holds: the shared view restricted to what that connection
observes, composed with that connection's overlay. Complete only on the client.
Kept on the server as the shared view, one overlay per connection, and each
connection's position in the history.

## Audio relation

The directed relation over connections deciding who may receive whose voice.
Declared outright, independently of the channel tree and of scopes. See
[Audio as a relation](audio.md).

## Render

One evaluation of the application logic, producing the shared view, the overlays
and the audio relation. Evaluated once per shard after a change in application
state, never once per connection.

## Delta

The operations separating two consecutive shared views: channels created, updated
or removed, participants added, moved, updated or removed. See
[Deltas and per-connection composition](deltas.md).

## Journal

The bounded history of deltas kept by a shard. Each connection holds a position
in it. A connection left behind the oldest retained entry is closed rather than
served a view that has drifted.

## Application logic

The code supplying the three operations a shard calls: the render, the set of
scopes a given connection observes, and the reaction to a client event. Written
as an implementation of the `ShardLogic` trait, and the only place the domain
rules live.

## Application state

Matches, teams, roles, positions, permissions derived from game rules. Owned by
the application and never inspected by the runtime.
