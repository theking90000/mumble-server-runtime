# The control plane

The control plane is one TLS connection carrying framed protobuf messages. It
survives a migration between shards, so a client holds it for the whole of its
stay.

## From accept to a usable session

```text
  client                                          server

  TCP connect ─────────────────▶
                ◀───────────── TLS 1.2 handshake ─────────────▶
                ◀───────────── Version
  Authenticate ────────────────▶
                                 route the connection
                ◀───────────── CryptSetup, CodecVersion
                ◀───────────── the first view
                ◀───────────── ServerSync, ServerConfig
                                 service loop
```

The server announces its version as soon as TLS completes, before authentication
rather than in answer to it. Anything the client sends before `Authenticate` is
accepted and ignored.

Routing happens between authentication and the first view. It is the one moment
a connection identifier and the claim behind it meet: a name, a credential and a
certificate hash go in, and either a shard or a refusal comes out. See
[Anatomy of an application](../build/anatomy.md).

## The first view is not a special path

The channels and participants sent between `CodecVersion` and `ServerSync` are
not a handshake-specific flood. They are the ordinary first transition of the
shard the connection was routed to, drained from the same queue that carries
every later transition.

One consequence is worth stating: there is exactly one rendering path in the
runtime, so a defect in it cannot hide behind a handshake that agrees with
nothing else.

Two orderings inside that transition are load-bearing. A parent channel is
created before its children, and a connection is introduced to itself before any
other participant, because the client looks its own session up when
synchronisation arrives.

The handshake waits five seconds for that first view. A shard that has not
published one by then loses the connection rather than leaving a client
synchronised against nothing.

## Refusals

A connection is refused with a reason in two cases: the advertised user ceiling
is reached, or the application declines to route it. The reason reaches the
client, which displays it.

## What synchronisation advertises

`ServerSync` carries the connection's own session, the welcome text and the
permissions it holds where nothing forbids them. `ServerConfig` carries the
bandwidth ceiling, the message length limit, the user ceiling, whether markup is
accepted and whether recording is allowed.

Two of those are advertised rather than enforced. The bandwidth ceiling is sent
and not applied server-side. The recording flag is advisory, and a client
announcing that it records is refused like any other unmirrored state change.

Default values are listed in
[Configuration and limits](../build/configuration.md).

## Ping

A client ping is answered with its own timestamp and the connection's
decryption counters. Reporting those counters is not bookkeeping.

The client reads the count of accepted datagrams to decide whether its UDP path
works. A count still at zero twenty seconds in makes it fall back to the TCP
tunnel permanently, whether or not UDP was working in both directions. A server
that answered pings without those counters would push every client onto the
tunnel.

Resynchronisation counters always report zero, which is their true value: nonce
resynchronisation is not implemented, and a resynchronisation request is
refused.

## Transport

TLS 1.2 only. A widely deployed client build crashes in its post-handshake
introspection when handed a TLS 1.3 session, and the reference server negotiates
1.2 by default.

A client certificate is requested and not required. When one is present, it is
checked to parse and nothing more: a self-signed certificate proves identity,
not authority. Its fingerprint reaches the application when the connection is
routed, and what that identity is worth is the application's decision.

Certificate generation exists for local development. A deployment supplies a
persistent certificate instead. Clients key their per-server preferences on it,
so a certificate regenerated at every restart looks like a new server each time.

## Framing

Messages are reassembled across TLS record boundaries, and a declared length
beyond the protocol maximum is an error. A connection that ends in the middle of
a frame is closed rather than resynchronised.
