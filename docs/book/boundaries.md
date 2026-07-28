# Boundaries

What the runtime does not do, and what is fixed rather than configurable. Stated
early, because most of it follows from one decision: a client asks, and only the
application decides.

## Nothing is driven from the client

The voice state comes from the render and from nowhere else. Control messages
that would change it directly are refused, with a denial carrying a reason:

- creating, editing or removing a channel,
- removing a participant, and moderation generally,
- editing access control lists,
- registering whisper or shout targets,
- requesting avatars, comments or other blobs,
- renegotiating the connection's encryption.

A client requesting entry to a channel is reporting an intention. The move
happens when the application renders it, or never.

Permissions are not a stored model either. The flag marking a channel as
enterable is a display hint, and an actual join is validated again when it
arrives.

Messages are rate-limited to one per second with a burst of five, the same
allowance as the reference server. Anything beyond that is refused with a reason
rather than queued.

## No domain knowledge

Matches, teams, roles, positions and the rules deriving from them stay in the
application. None of it is inspected, stored or interpreted by the runtime.

Nothing outlives the process on the runtime's side. Channels and participants
exist as the current render produces them, so persistence is the application's
concern alone.

Positional audio is not provided. Position may still reach the client through
Mumble Link, which is a separate and local mechanism.

## Audio

Opus only. The payload is never decoded, only forwarded, so codec conversion and
server-side mixing are out of scope.

Positional data present in an incoming packet is stripped before forwarding.
Passing on coordinates the application never authorised would leak position.

Rate limits on voice are per connection and fixed: a sustained rate with a short
burst allowance, and a maximum packet size. A connection exceeding them has
packets dropped rather than being disconnected.

## Fixed in the model

None of these is settable from application code.

| limit                                  | value         |
| -------------------------------------- | ------------- |
| scope depth                            | 4 segments    |
| scopes observed per connection         | 4             |
| delta history retained                 | 256 versions  |
| context actions offered per connection | 64            |
| output queue per connection            | 1024 messages |
| minimum interval between publications  | 50 ms         |

Each bound has a cost behind it rather than a preference. Exceeding one refuses
the render, except for the queue, whose overflow closes the connection rather
than letting its view drift.

## Configuration

There is no configuration file and no environment variable. Everything is Rust
code: the bind address, the advertised user ceiling, the welcome text, the
message length limit, whether HTML is allowed in messages, and the advertised
version.

Two of those are advertised rather than enforced. The bandwidth ceiling is sent
to clients and not applied server-side. The recording-allowed flag is advisory.

## Transport

TLS 1.2 only, because a widely deployed client build crashes on a TLS 1.3
session.

A client certificate is requested but not required, and when present it proves
identity rather than authority. What that identity is worth is decided by the
application when a connection is routed.

Self-signed certificate generation exists for local development. A deployment
supplies a persistent certificate instead, otherwise every restart looks like a
new server to clients, which key their local preferences on it.
