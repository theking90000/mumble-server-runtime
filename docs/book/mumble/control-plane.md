# The control plane

Connecting a Mumble client looks ordinary from the client's side: a TLS
connection, a login, a channel tree, and a session that stays usable until the
client leaves. This page covers what happens in that sequence, and the few
places where the outcome depends on something the client cannot see.

The connection survives a migration between shards, so a client holds one for
the whole of its stay.

## From connect to a usable session

```text
  client                                          server

  TCP connect ─────────────────▶
                ◀───────────── TLS 1.2 handshake ─────────────▶
                ◀───────────── Version
  Authenticate ────────────────▶
                                 route the connection
                ◀───────────── CryptSetup, CodecVersion
                ◀───────────── the channel tree
                ◀───────────── ServerSync, ServerConfig
                                 the session is live
```

The server announces itself as soon as TLS completes, before the login rather
than in answer to it. Anything the client sends before logging in is accepted
and ignored.

Routing happens between the login and the tree. A name, a password and a
certificate fingerprint go in, and either a shard or a refusal comes out. It is
the one moment where a connection and the claim behind it meet, and the only
place an application decides who a connection is. See
[Anatomy of an application](../build/anatomy.md).

## The tree the client receives

The channels and participants that arrive before synchronisation are not a
login-specific sequence. They are the first ordinary transition of the shard the
connection was routed to, sent through the same path as every later change.

For a reader that means the tree a client receives on connecting and the tree it
holds an hour later are produced identically. There is no separate startup path
that could agree with nothing else.

Two orderings inside it matter to the client. A parent channel arrives before
its children, and a connection is introduced to itself before any other
participant, because the client looks its own session up when synchronisation
arrives.

A shard that publishes nothing within five seconds loses the connection, rather
than leaving a client synchronised against an empty view.

## Refusals

A connection is refused with a reason in two cases: the advertised user ceiling
is reached, or the application declines to route it. The reason reaches the
client and is displayed.

## What the session advertises

Synchronisation carries the connection's own session, the welcome text and the
permissions it holds where nothing forbids them. The configuration that follows
carries the bandwidth ceiling, the message length limit, the user ceiling,
whether markup is accepted and whether recording is allowed.

Two of those are advertised rather than enforced. The bandwidth ceiling is sent
and not applied. The recording flag is advisory, and a client announcing that it
records is refused like any other unmirrored change.

Defaults are listed in [Configuration and limits](../build/configuration.md).

## Latency, and one trap

A client ping is answered with its own timestamp and the connection's decryption
counters.

Those counters decide something the client never asks about directly. A Mumble
client reads the count of accepted datagrams to judge whether its own UDP path
works, and a count still at zero twenty seconds in makes it fall back to the TCP
tunnel permanently, whether or not UDP was working in both directions.

Resynchronisation counters always report zero, which is their true value.

## Transport

TLS 1.2 only. A widely deployed client build crashes in its post-handshake
introspection when handed a TLS 1.3 session, and the reference server negotiates
1.2 by default.

A client certificate is requested and not required. When present, it is checked
to parse and nothing more: a self-signed certificate proves identity, not
authority. Its fingerprint reaches the application when the connection is
routed, and what that identity is worth is the application's decision.

A deployment supplies a persistent certificate. Clients key their per-server
preferences on it, so a certificate regenerated at every restart looks like a
new server each time, and every client loses its local settings. Generation
exists for local development, where that does not matter.
