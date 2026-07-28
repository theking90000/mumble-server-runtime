# Refusals

Nothing a client sends changes voice state on its own, and nothing incoherent is
sent to a client. Both properties come from refusing early, in three distinct
places.

## Client events are statements, never commands

An event reported to the application describes what a client asked for:
connecting, disconnecting, requesting entry to a visible channel, requesting
self-mute or self-deafen, invoking a menu entry the application itself defined,
sending a message.

No voice state is changed by delivering one. Application state is updated when a
request is accepted, and the consequence appears through the next render.
Ignoring an event entirely is a valid answer.

A refusal therefore consists of rendering nothing new. A message may be returned
to the connection alongside it, which reaches the client as a denial carrying a
reason, but the message itself changes nothing.

## Events arrive pre-resolved and pre-validated

An event is expressed in the application's own vocabulary, using channel keys and
occupants rather than wire numbers, and it has already been checked against what
that connection can see.

A request naming a channel that connection cannot see is dropped and logged
rather than answered, so a guessed number cannot be used to test whether
something exists. An invoked menu entry is checked against what the client
actually holds rather than what the last render intended, and against the places
the entry was declared for. A message aimed at a visible but read-only channel is
refused out loud.

## A refused render keeps the previous view

A render is accepted or discarded whole. Nothing partial reaches a client, and a
refusal closes no connection: the committed view is still correct, and
reconnecting would only reproduce the same failure.

The refusals fall into four families.

**Structure.** No root, two roots, one channel key or one occupant rendered
twice, a link between channels whose scopes are not comparable, identifiers
exhausted.

**Scope.** Narrowing past the maximum depth. Refused rather than clamped, since
clamping would hand the element its parent's visibility.

**Shared against private.** A channel or a participant present both in the shared
view and in an overlay. An overlay placing someone in a channel absent from the
render, or in a channel that connection cannot see.

**Audio.** An edge whose receiver cannot see the sender.

More context actions than fit in one transition is also refused, for a mechanical
reason: the whole turn enters the connection's queue in one piece, and an
oversized batch would close the connection instead of failing inside application
code.

## Failing closed

Where a declaration is incomplete, the result is silence rather than exposure. A
listen naming a domain that was never declared yields no route. An unimplemented
path returns an explicit denial rather than doing something approximate.

The bias is deliberate and consistent: a mistake costs a missing feature, never a
leak.
