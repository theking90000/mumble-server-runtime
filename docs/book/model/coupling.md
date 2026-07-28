# Seeing the speaker

> A receiver must see the speaker.

This is the single point where visibility and audibility are not independent, and
it does not originate in Voxloom.

## A protocol constraint

Before buffering a voice frame, the Mumble client looks up the sender's session
in the model it holds. A frame whose sender is unknown is discarded.

An audio edge landing on a receiver who cannot see the sender would therefore
deliver nothing. It is not a policy question but a fact about the client, and
declaring such an edge is silence with extra steps.

A corollary worth stating: there is no separate right to speak. Speaking
somewhere implies being visible there.

## Checked on the render's output

The condition is verified when a render is finished, against what each connection
can see. A render declaring an edge that violates it is refused whole, and the
previous view stays in place.

Visibility here means either route: through the shared view, when the sender's
scope is comparable to one the receiver observes, or through that receiver's own
overlay. Someone placed by an overlay counts as being there.

## Two orderings carry the rule at runtime

The rule holds on the render's output, but a transition takes time to reach a
client, so two orderings keep it true in between.

**The routing table is published before any view.** Revocation therefore takes
effect immediately, which is always safe: cutting a route early costs a lost
packet, never a leak.

**A granted route stays inert until the receiver has been told.** Each speaker in
the table carries the version at which it became visible, and a receiver whose
position in the history has not reached that version is given nothing. The gate
is one comparison on the voice path, with no shard involved.

**Control and voice share one queue per connection.** The message introducing a
speaker is therefore written before that speaker's tunnelled audio, rather than
racing it.

## Text inherits the same rule

A recipient who cannot see the speaker is skipped when a message is relayed. A
text message naming a session the recipient does not hold would break the same
client-side assumption that audio breaks, so the constraint is not specific to
voice.
