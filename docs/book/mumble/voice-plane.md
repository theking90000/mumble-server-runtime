# The voice plane

A client speaks, and the people the application put within earshot hear it. This
page covers what happens between those two facts, and the handful of cases where
a client behaves differently from what its own interface suggests.

## Opus, carried and not opened

The audio a client sends is Opus, and it is forwarded exactly as it arrived.
Nothing decodes it, so there is no mixing, no transcoding and no server-side
analysis of what anyone said.

The codec announcement sent before the first view leaves Opus alone and disables
the legacy codecs, which is the reference server's own reset state.

## Two ways to the server, chosen by the client

Voice normally travels over UDP. A client whose UDP path does not work sends its
voice through the control connection instead, and receives it the same way.

Neither side negotiates the switch. A client that tunnels its voice is stating
that UDP failed, and is served through the tunnel from then on. Any datagram
arriving over UDP puts it back on the fast path.

The choice is per connection, so a client on UDP and a client on the tunnel hear
each other with nothing special happening. The sender's transport never enters
into how a receiver is served.

A client that has never sent a datagram has no UDP address on the server side.
Its address is learned from a datagram that decrypts under its own key, which
makes association a proof rather than a claim.

## Who hears whom

The application declares the audio relation, and the runtime applies it. See
[Audio as a relation](../model/audio.md).

One rule sits underneath and is not the application's to set: a receiver is
given a speaker's audio only once it has been told the speaker exists. A Mumble
client discards audio from a session it does not know, so delivering it earlier
would be delivering silence.

The check errs on the side of silence. A client lagging behind loses audio from
speakers it already knew about, at worst a fraction of a second. Hearing someone
absent from the view is never possible. See
[Seeing the speaker](../model/coupling.md).

Withdrawing audibility needs no such care. A route that is gone is simply
absent, and cutting early is always safe.

## Where speech goes

Two destinations are accepted: normal speech, and the loopback a client uses to
test its own microphone. Loopback is answered directly to the speaker and is
never a route to anyone else.

Every other destination is dropped. Registering a whisper or shout target is
refused, so a target the server never granted means nothing, and treating it as
normal speech would deliver voice to people the client never addressed.

## What is removed on the way

Positional data is stripped from every forwarded packet. Passing on coordinates
the application never authorised would tell everyone in earshot where the
speaker is. Position may still reach a client through Mumble Link, which is
local to each participant and never reaches the server.

The volume adjustment a sender attaches is cleared. How loud someone is heard is
not the speaker's decision.

## Before connecting

A connectivity ping is answered before any session exists, reporting the
version, the current and maximum user counts and the bandwidth ceiling. This is
how a client measures a server before connecting, and how a server list probes
one.

## Limits

Voice limits are fixed and per connection: a maximum packet size, a sustained
packet rate with a burst allowance, and a bandwidth window billed with the same
per-packet overhead the reference server uses.

A connection exceeding them has packets dropped rather than being disconnected,
and both paths into the server share one rule, so tunnelling voice is not a way
around it.

Voice pushed onto a saturated queue is dropped. A gap is preferable to voice
arriving late. A datagram that fails to decrypt is dropped and counted; there is
no resynchronisation.
