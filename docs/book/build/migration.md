# Migration between shards

A migration hands one connection from one shard to another without interrupting
it. The client keeps its TCP connection, its TLS session, its session identifier
and every local setting attached to them. The world changes around it.

## Asking for one

```rust
out.switch(connection, arena);
```

Called from `observe`, like every other answer. It records an effect the runtime
carries out, since orchestrating two shards is beyond what one shard can do. A
shard belonging to no runtime says so rather than pretending the move happened.

Anything said in the same breath is delivered first, so a farewell reaches the
socket before the move is asked for.

## The order

```text
  source shard                 runtime                destination shard

  Detach { handover }  ──────────▶
  compose the held view
  the client's view    ──────────▶
                            move the routing table
                                  ──────────▶  Attach { held }
                                               plan one transition
```

Three orderings are load-bearing.

The held view is composed by the source and handed over before the destination
plans anything. Without it the destination would be planning from a guess. If
the source is gone, the connection is closed instead, because what the client
holds is then unknown.

The routing table follows the connection before its view does. A route it is no
longer entitled to disappears immediately, and a route it gains stays inert
until its cursor catches up. Cutting early is always safe, the reverse never is.

Context actions the source offered are withdrawn as it lets go. An action
belongs to the shard that offered it, and the destination starts from an empty
registry. Otherwise the client would keep buttons nobody can withdraw, and
invoking one would reach a shard that never granted it.

## Why not a disconnect and a reconnect

Tearing down first does not merely waste work, it disconnects the official
client.

The client keeps itself in its own model after a `UserRemove` naming its own
session. The `ChannelRemove` that follows therefore looks to it like the server
removing an occupied channel, which it treats as a protocol violation.

## Attaching is an ordinary scope change

The destination receives the connection observing nothing and holding a view
some other shard produced. The next turn takes the slow path, sees a connection
holding a view this shard did not produce, and plans one transition onto its own
tree.

That is the same path a scope change takes, so attach, detach and migrate are
one mechanism rather than three. Whatever the two trees have in common does not
flicker, and the no-flicker property comes free rather than being maintained.

## What each side observes

The source receives `Migrated { connection, to }`. The destination receives
`Connected { connection }`.

`Migrated` is deliberately not `Disconnected`. A disconnect frees a slot, a
migration hands it over, and an application that conflates them will decrement a
count it should have transferred.

## Carrying state across

A shard learns nothing about a connection beyond its `ConnectionId`. State that
has to survive a move therefore lives in the application, shared by both shards:

```rust
let directory = Arc::new(Directory::new());
let chosen = Arc::new(Choices::new());

let lobby = runtime.create_shard({
    let directory = Arc::clone(&directory);
    let chosen = Arc::clone(&chosen);
    move |_handle| Lobby::new(directory, chosen)
});
let arena = runtime.create_shard(move |_handle| Arena::new(directory, chosen));
```

The demonstration application records a name and a staff flag in the router, and
the side a player picked in the lobby. The arena reads both without either shard
knowing the other exists.

## Both ends have to exist

A move naming a shard that does not exist is refused and logged, and the
connection stays where it is. Creating every shard a router or a logic can name
before the first connection is accepted avoids the case entirely, which is what
the closure passed to `serve` is for.

## Scope

Shards are units of rendering inside one runtime, not separate machines. Moving
a connection between them is implemented and covered by tests. Spreading shards
across hosts behind voice proxies is designed and not built.
