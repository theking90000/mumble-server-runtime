# The shared view and scopes

A _scope_ is a position in a tree, at most four segments deep. Every rendered
element occupies one. Every connection observes a set of at most four.

Segments are opaque numbers. Their meaning belongs to the application: a match
identifier, a team identifier, a region.

## Derived, never declared

The builder has no scope parameter. A scope is derived from the element it hangs
under, plus one instruction:

- `Narrow::Same` keeps the parent's scope.
- `Narrow::Into(segment)` descends one segment below it.

A channel's scope is derived from its parent's, a participant's from that of the
channel it is rendered in, the shard's root from the root scope.

Narrowing is the only available direction. There is no widening instruction, and
beyond four segments the operation is refused rather than saturated. Saturating
would hand a child its parent's visibility, which is a disclosure dressed as a
rounding error. The whole render is refused instead, and the previous view stays
in place.

## Visibility

A connection sees an element when one of its observed scopes is _comparable_ to
the element's, meaning one is a prefix of the other.

Both directions count. An observer at `/g7/t2` sees its ancestors, `/g7` and the
root, along with everything below `/g7/t2`. An observer at `/g7` sees `/g7/t2`
and `/g7/t3` alike. Moving up the tree is how an observer comes to see more.

```text
  scope tree            observer at that scope    sees

  /                     staff                     every scope below
  |
  +- /g7                spectator                 /, /g7, /g7/t2, /g7/t3
     |
     +- /g7/t2          red player                /, /g7, /g7/t2
     |
     +- /g7/t3          blue player               /, /g7, /g7/t3
```

The red player and the blue player do not see each other. Comparability is
reflexive and symmetric, and deliberately not transitive: `/g7/t2` and `/g7/t3`
are both comparable to `/g7` and not to each other. That is what keeps two teams
apart while a spectator above them sees both, with nothing added to the model.

## The closure property

A channel's scope extends its parent's and a participant's extends its channel's.
One property follows without a runtime check: seeing an element implies seeing
what that element refers to.

Writing `c` for a channel's scope and `u` for a participant's, `c` is a prefix of
`u`. An observer seeing the participant does so through some scope `s` comparable
to `u`, which leaves two cases. If `s` is a prefix of `u`, then `s` and `c` are
two prefixes of one path and therefore comparable. If `u` is a prefix of `s`,
then `c` is a prefix of `s`. The channel is visible either way.

A participant is therefore never visible in an invisible channel, and a channel
never visible without its parent. There is no consistency check to run and no
failure to handle, because the incoherent case cannot be expressed.

## A scope is not a channel

The two trees are independent. One channel may hold participants of several
scopes, each visible to a different set of observers.

The rule tying them is extension, not equality, so a channel left at the root
scope may hold participants narrowed into their respective teams. Each player
then sees the channel and, inside it, only their own team.

## Bounds

Four segments of depth. Four observed scopes per connection. Both are refused
past the limit rather than clamped.

Neither bound is advisory. A scope is a copied value consulted once per
connection per turn, so an unbounded path would require an allocation on the
composition path, and the quadratic cost this model exists to avoid returns as
soon as the observation bound is lifted.

## Choosing a scope

A scope describes a group. An individual exception belongs in an
[overlay](overlays.md) instead. A scope with one observer is an overlay in
disguise.

A role is a group by nature even while it holds a single member: player, host,
spectator, staff. A vanished administrator is not a role but one person's
exception.
