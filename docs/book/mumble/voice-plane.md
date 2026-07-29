# The voice plane

Audio travels over UDP, encrypted per connection, and falls back to the control
connection when UDP does not work. No shard takes part in it: a shard publishes
a routing table, and the voice plane reads it.

## Opus, never decoded

The payload is Opus and is forwarded verbatim. Decoding it would only be needed
for mixing, transcoding or content analysis, none of which happen here.

The codec announcement sent before the first view disables the legacy codecs and
leaves Opus alone, which is the reference server's reset state.

## Association

A connection's UDP address is learned from a datagram it sends, and only from
one that decrypts under its own key. Association is therefore a cryptographic
proof rather than a claim, and a client that never sends a datagram has no UDP
address on the server side.

Until that proof arrives, the connection has no UDP path and is served through
the tunnel.

## The path of a packet

```text
  1. receive the datagram
  2. find the connection that owns the source address
  3. decrypt under that connection's key alone
  4. read the routing table its shard published
  5. per receiver, re-encrypt under the receiver's key and send
```

Each connection holds its own cipher state, so a packet is decrypted once and
re-encrypted once per receiver. A sender never learns who receives it.

## A receiver hears only what it can see

A receiver is given a speaker's audio only once it has been told the speaker
exists. A client discards audio from a session it does not know, so delivering
it earlier would be delivering silence.

The check is conservative on purpose. A receiver lagging behind its shard loses
audio from speakers it already knew about, which is at worst a fraction of a
second of silence for a client already in trouble. The reverse, hearing someone
who is not in the view, is never possible. See
[Seeing the speaker](../model/coupling.md).

Revocation needs no check at all. A shard publishes its routing table before it
pushes any view, so a route that is gone is simply absent. Cutting early is
always safe.

## The tunnel

A connection with no usable UDP path receives its audio through the control
connection instead.

The switch happens in both directions without negotiation. A client that sends
its voice through the control connection is stating that its UDP does not work,
and is served through the tunnel from then on. Any datagram arriving over UDP
puts it back on the fast path.

Delivery branches per receiver, so a client on UDP and a client on the tunnel
hear each other with no special case: the sender's transport never enters into
the decision.

Voice pushed onto a saturated queue is dropped rather than queued. A gap is
preferable to voice arriving late.

## Targets

Two targets are accepted: normal speech, and the local loopback a client uses to
test its own microphone. Loopback is answered directly and is not a route, since
the audio relation never contains an edge from a participant to itself.

Every other target is dropped. Registering a voice target is refused, so a
target the server never granted means nothing, and routing it as normal speech
would deliver voice to receivers the client never addressed.

## What is stripped

Two fields are cleared on every forwarded packet.

Positional data is removed. Forwarding coordinates the application never
authorised would leak position to everyone in earshot. Position may still reach
a client through Mumble Link, which is local to each participant.

The volume adjustment a sender attaches is cleared. Audibility is decided by the
application, not by the speaker.

## Pings before anything

A connectivity ping is answered before any association exists, in plaintext,
reporting the version, the current and maximum user counts and the bandwidth
ceiling. This is how a client measures a server before connecting to it, and how
a server list probes one.

## Limits

Voice limits are fixed and per connection: a maximum packet size, a sustained
packet rate with a burst allowance, and a bandwidth window billed with the same
per-packet overhead the reference server uses.

A connection exceeding them has packets dropped rather than being disconnected.
Both entry paths share one rule, so tunnelling voice is not a way around it.

A datagram that fails to decrypt is dropped and counted. There is no
resynchronisation.
