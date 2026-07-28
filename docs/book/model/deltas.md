# Deltas and per-connection composition

A shard renders once. What each connection receives is the difference between two
consecutive shared views, filtered down to what that connection observes.

```text
  application state changes
       |  wake
       v
  render ──▶ plan ──▶ journal ──▶ per connection: filter, splice, collapse
```

## The scope belongs to the identity

The comparison key of the diff is the pair of element and scope, not the element
alone. That choice costs nothing and removes a family of special cases.

A player changing team moves from `/g7/t3` to `/g7/t2`. Under the pair key this
is not a field that changed. The entry at `/g7/t3` disappears and the entry at
`/g7/t2` appears, so an ordinary diff produces two operations on its own:

```text
  RemoveUser(B)               [scope /g7/t3]
  AddUser(B, channel Team2)   [scope /g7/t2]
```

The per-connection filter then stays one test with no branches:

| connection | observes | receives |
|---|---|---|
| teammate still in t3 | `/g7/t3` | the removal alone, so B leaves |
| player in t2 | `/g7/t2` | the addition alone, so B arrives |
| spectator | `/g7` | both, settled by `collapse` |
| player in another match | `/g8` | nothing |

A property follows for free: a move inside one scope stays a move, while a move
between scopes becomes a departure and an arrival. The identifier does not
change, since it is the same person and the client's local preferences for them
have to survive. Only the diff's notion of sameness carries the scope.

## Phase order

The operations of one transition are grouped so that the ordering rules the
protocol depends on hold by construction:

```text
  P1  CreateChannel      parents before children
  P2  UpdateChannel
  P3  AddUser
  P4  MoveUser           before any removal, so no occupied channel is deleted
  P5  UpdateUser
  P6  RemoveUser
  P7  RemoveChannel      children before parents
```

Audio produces no operation here. The routing table is separate and its ordering
is guaranteed differently, so a plan is purely a sequence of view changes.

## The journal

Each delta is appended under a new version number. A connection holds a position
in that history and replays the slice it has not seen.

The history is bounded to 256 versions. A connection whose position has fallen
below the oldest retained entry cannot be repaired from deltas and is closed; it
reconnects from nothing.

No snapshot mechanism exists, and none is needed. A newcomer receives the plan
from an empty view to the current one, which is the ordinary planner. A departure
is the inverse. A migration is both.

## Composition

One connection's transition is built in four lines:

```text
  shared  = filter(journal.replay(cursor, head), observed)
  private = plan_elements(overlay_sent, overlay_new)
  ops     = splice(shared, private)
  collapse(ops)
```

**Filter** keeps the operations whose scope this connection observes. Validity is
preserved for free: every ordering rule has the form "X before Y", and dropping
elements from a sequence violates none of them, so a valid plan stays valid after
filtering. The closure property does the real work, guaranteeing that no
reference is left dangling by the elements dropped.

**Splice** inserts the overlay's operations inside the shared phases rather than
after them. The counter-example is immediate: with private operations last, a
shared delta removing a channel would send that removal before the administrator
standing in it is withdrawn, deleting an occupied channel.

```text
  P1..P5   shared additions, filtered
           ├─ overlay removals     before: they vacate a channel about to die
           └─ overlay additions    after:  they may target a brand new channel
  P6..P7   shared removals, filtered
```

**Collapse** drops every removal of an element that is also added in the same
transition. One pass covers three situations, because in all three the answer is
the same: an element that has an addition somewhere still exists, so the removal
is wrong. A full addition carries the element's complete state, and channel and
participant state are merged rather than replaced on the client, so re-sending an
element whole amounts to updating it.

| situation | operations produced | after collapse |
|---|---|---|
| the element changes scope | removal, then addition | the addition alone |
| vanished to visible | removal from the overlay, addition to the shared view | the addition alone |
| visible to vanished | removal from the shared view, addition to the overlay | the addition alone |
| dropped everywhere | removal alone | the removal, kept |

## What is deliberately not done

Diffing each connection's composed view against what it currently holds is the
honest formulation, and it is always correct. It also costs the size of a full
view per connection, which is the quadratic this whole design exists to escape.

Composition instead costs the size of the delta, plus the size of that
connection's overlay, plus the size of the result.
