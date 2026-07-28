# Cost

The model exists for one reason, and the reason is a cost.

## The shape being avoided

Rendering the whole world as seen by one connection, once per connection, means
N connections each holding a view of size proportional to N. Materializing every
view then costs N squared, and no scheduler and no cache change that, because the
cost is in the shape of the operation rather than in its implementation.

A shard renders once instead. Each fact is held once, whoever ends up seeing it,
and what connections receive is the resulting difference, filtered.

## What one turn costs

Writing `W` for the size of the shared view, `N` for attached connections and
`|D|` for the size of the delta:

```text
  per shard, per turn =
        O(W)                          the shared render, once
      + O(N × |D|)                    the filter, one pass per connection
      + Σ |overlay|                   the individual exceptions
      + O(W) per connection whose observation changed
      + O(N + edges)                  when the audio relation changed
```

What costs is the size of each difference, not the number of connections
receiving one. A three-operation delta filtered N times costs `O(N × |D|)`, not
`O(N × W)`, and the property holds even when every view differs. That is what
makes it robust rather than merely fast.

## Per mechanism

| mechanism | cost |
|---|---|
| shared view and scopes | `O(W)` once, plus `O(\|D\|)` per connection |
| private overlay | the size of the overlay |
| audio relation | `O(N + edges)` |

The audio primitives differ among themselves: a domain is quadratic in its own
size, a listen is linear in the size of the domain listened to, an edge is
constant.

## Where the bounds come from

The fixed limits in the model are all consequences of this cost, not matters of
taste.

Scope depth is bounded because a scope is a copied value consulted once per
connection per turn, and a heap-allocated path would put an allocation on the
composition path. The number of scopes one connection observes is bounded for the
same reason, and because an unbounded observation restores the quadratic term
directly.

The journal is bounded because it is memory held for connections that have fallen
behind. Past that distance a connection is unrecoverable from deltas anyway.

The number of context actions offered to one connection is bounded because a turn
enters that connection's queue in one piece, and an oversized batch would close
the connection rather than fail inside application code.

## The known spike

A change of observation costs a full replan for the connection concerned. When
many observations change at once, at the start of a round for instance, the term
that is normally zero becomes `O(W)` per affected connection.

Two things bound it. Renders are coalesced under a minimum interval, so a burst
of state changes yields one turn rather than many. And sharding per match keeps
`W` to one match rather than one server.

## Refusing to trade the property away

Diffing each connection's composed view against what it holds is correct and
obvious, and it costs `O(N × W)`. It is the formulation this design exists to
avoid, and no optimisation recovers what adopting it gives up.
