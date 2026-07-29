# What a client can do

An unmodified Mumble client connects, moves between the channels it can see,
talks, types, and uses the menu entries an application offers it. It cannot
create a channel, kick anyone, whisper, or display an avatar.

The line between the two is the one drawn in [Boundaries](../boundaries.md).
Anything that would change the voice session directly is refused, because the
session is rendered. Anything that reports an intention is forwarded to the
application, which decides.

## What works

| the client does | result |
| --- | --- |
| connect | accepted up to the ceiling; name and password reach the application |
| enter a channel it can see | forwarded as a request, granted by the next render |
| talk | delivered to the receivers the application declared |
| mute or deafen itself | forwarded as a request, shown to others once rendered |
| type into a channel, or to one person | relayed if the application accepts it |
| use a menu entry offered to it | forwarded, along with what it was invoked on |
| ask what it may do in a channel | answered from the view it currently holds |
| ask for its own statistics | answered in full |
| ask about someone else | answered with a name and nothing more |
| measure latency | answered, with the counters its transport needs |

Entering a channel is the one worth reading twice. The client reports that it
would like to move. Nothing has happened when the request arrives, and the move
becomes real when the application renders it, or never. See
[Client interactions](../build/interactions.md).

## What is refused

| the client tries to | why |
| --- | --- |
| create, rename, move or remove a channel | the tree is rendered, never edited |
| kick, ban or move another participant | moderation belongs to the application |
| edit access control lists | there is no stored permission model |
| register a whisper or shout target | audio destinations come from the render |
| set an avatar, a comment or a texture | no blob is stored or served |
| register an account, or list registered users | nothing outlives the process |
| announce that it is recording | the recording flag is advisory only |
| renegotiate its encryption | resynchronisation is not implemented |

A refusal reaches the client as a denial it displays. Nothing is acknowledged
silently: a change that will never appear in any view is refused rather than
accepted and forgotten.

Two exceptions match the reference server. A message over the rate limit is
dropped without an answer, because answering a flood is participating in it. An
empty message is dropped because there is nothing to deliver.

## What the client shows as a result

The permissions advertised to a client cover traversal, entry, speech and text.
Channel and administration rights are deliberately withheld, so the
corresponding menus do not appear at all rather than appearing and failing.

Avatars and comments stay empty, since the requests that would fetch them are
refused. Whisper and shout keys produce nothing, since the target they would
address was never registered.

Permissions are not a stored model. A question about a channel is answered from
the render that produced it, and an entry is validated again when it arrives.

## Text

A typed message is dropped if it arrives faster than one per second with a burst
of five, if it is empty, if it is longer than the advertised limit, or if it
carries markup on a server configured to refuse it. A server that forbids markup
refuses it rather than rewriting it, because stripping markup correctly means
running a parser on client input.

Those four are properties of the message. Whether the connection may address
that channel or that person at all is a separate question, answered by the
application. See [Refusals](../model/refusals.md).

## The messages behind it

Every Mumble message type decodes, and an unknown type code or a malformed
payload is an error rather than something silently accepted. What varies is what
happens next.

| inbound | handling |
| --- | --- |
| `Authenticate` | opens the session: name, credential, certificate hash |
| `UserState` | two intents only: channel entry, self-mute and self-deafen |
| `TextMessage` | relayed, subject to the four checks above |
| `ContextAction` | forwarded to the application |
| `PermissionQuery`, `UserStats` | answered from the published view |
| `Ping` | answered with the timestamp and the decryption counters |
| `UdpTunnel` | carried as voice |
| `Version` | accepted and ignored, like any pre-authentication traffic |
| anything else | refused, and logged |

A name longer than 64 characters is truncated, and an absent one becomes
`Guest`. The password field is an opaque credential: nothing is inspected, and
what it is worth is decided when the connection is routed.

A `UserState` naming another session is refused, as is one carrying a recording
announcement, a plugin context, a listener registration or an access token.

| outbound | when |
| --- | --- |
| `Version` | after the TLS handshake, before authentication |
| `CryptSetup`, `CodecVersion` | before the first view |
| `ChannelState`, `ChannelRemove` | a channel enters, changes or leaves the view |
| `UserState`, `UserRemove` | a participant enters, changes or leaves the view |
| `ServerSync`, `ServerConfig` | the view is complete, the session is usable |
| `Reject` | the connection is refused, with a reason |
| `Ping`, `PermissionQuery`, `UserStats` | in answer to a client, never unprompted |
| `TextMessage` | a message the application relayed |
| `ContextActionModify` | a menu entry is offered or withdrawn |
| `PermissionDenied` | any refusal |
| `UdpTunnel` | voice for a connection with no usable UDP path |

Nine types are never sent: `Acl`, `BanList`, `QueryUsers`, `UserList`,
`VoiceTarget`, `SuggestConfig`, `PluginDataTransmission`, `RequestBlob` and
`ContextAction`.

Pings, permission queries, statistics requests, codec announcements, blob
requests and target registrations do not count as activity. A client sending
nothing else is idle, the same accounting the reference server applies.
