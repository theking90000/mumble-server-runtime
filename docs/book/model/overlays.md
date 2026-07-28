# Overlays

An _overlay_ is the set of elements visible to exactly one connection: channels
and participants held apart from the shared view, and recomputed from scratch on
every render.

Scopes describe groups. Overlays describe individual exceptions: a vanished
administrator, a private channel one person holds, a participant shown to a
single observer in a place nobody else sees.

## Not an override layer

An element is shared or private, never both. A channel or a participant present
in the shared view and in any overlay refuses the render.

Composition is therefore a disjoint union rather than a layering. No field of a
shared element can be replaced through an overlay. An overlay carries only
elements the shared view does not.

The exclusion is worth a refusal because of what the alternative permits. With
both allowed, the shared view could say the administrator stands in the staff
channel while an overlay says the same administrator stands in a player's
channel. The next shared delta would then move the administrator without the
overlay reasserting itself, and the client would drift with nothing left to
detect it. Silent divergence is the worst failure this design can produce.

## Vanishing is absence, not suppression

An invisible participant is not a visible participant hidden after the fact. It
is a participant left out of the shared view entirely, and placed in the overlay
of whoever should see them.

The same shape appears in the other direction. Announcing one person to a single
observer means leaving them out of the shared view and adding one overlay entry
per observer who should receive them.

## References stay inside what is already visible

An overlay may refer only to what its connection can already see. A participant
placed by an overlay sits in a channel that is either in that same overlay, or in
the shared view and visible to that connection. The same holds for the parent of
a channel an overlay adds.

One lookup per overlay element rules out three failures in a single stroke:
placing someone in a channel about to disappear, placing them in a channel that
connection cannot see, and referring to a session that does not exist.

## Cost and lifetime

An overlay is never journalled. On each turn it is recomputed, compared against
the one last sent to that connection, and the difference joins that connection's
transition.

What is paid is the size of the overlay, not the size of the view. Overlays stay
cheap as long as they stay exceptional, which is the same reason a scope with a
single observer should have been an overlay to begin with.
