# Supported surface

Every Mumble message type decodes. The protocol layer parses the whole register,
and an unknown type code or a malformed payload is an error rather than a
silently accepted message.

The narrowing happens one level up. A decoded message meets one of three fates:
acted on, refused with a reason, or accepted and ignored.

## What a client may send

| message | fate |
| --- | --- |
| `Version` | accepted and ignored, like any pre-authentication traffic |
| `Authenticate` | opens the session: name, credential, certificate hash |
| `Ping` | answered with the timestamp and the decryption counters |
| `UserState` | two intents only, see below |
| `PermissionQuery` | answered from the published render, for the asker alone |
| `UserStats` | about itself, in full; about anyone else, a name and nothing more |
| `TextMessage` | relayed, subject to four checks |
| `ContextAction` | forwarded to the application, which knows what it offered |
| `UdpTunnel` | carried as voice |
| anything else | refused, with the refusal logged |

A name longer than 64 characters is truncated, and an absent one becomes
`Guest`. The password field is an opaque credential: nothing is inspected, and
what it is worth is decided when the connection is routed.

## The two intents in `UserState`

A `UserState` naming its own session is read for exactly two things: a request
to enter a channel, and a change of self-mute or self-deafen. Both are forwarded
as requests.

A `UserState` naming another session is refused. So is one carrying a recording
announcement, a plugin context, a listener registration or an access token.
None of them is mirrored into a view, and acknowledging a change that will never
happen would be a lie.

## What the server sends

| message | when |
| --- | --- |
| `Version` | immediately after the TLS handshake, before authentication |
| `CryptSetup`, `CodecVersion` | before the first view |
| `ChannelState`, `ChannelRemove` | a channel enters, changes or leaves the view |
| `UserState`, `UserRemove` | a participant enters, changes or leaves the view |
| `ServerSync`, `ServerConfig` | the view is complete, the session is usable |
| `Reject` | the connection is refused, with a reason |
| `Ping` | in answer to a client ping |
| `PermissionQuery` | in answer to a query, never unprompted |
| `UserStats` | in answer to a request |
| `TextMessage` | a message the application relayed |
| `PermissionDenied` | any refusal |
| `ContextActionModify` | a menu entry is offered or withdrawn |
| `UdpTunnel` | voice for a connection with no usable UDP path |

Nine message types are never sent: `Acl`, `BanList`, `QueryUsers`, `UserList`,
`VoiceTarget`, `SuggestConfig`, `PluginDataTransmission`, `RequestBlob` and
`ContextAction`.

## Client features that do not work

Refusing a message is visible in the client, so the list is worth stating
plainly.

- **Channel administration.** Creating, renaming, moving or removing a channel
  is refused. The tree is rendered.
- **Moderation.** Kicking, banning, moving another participant and editing
  access control lists are refused.
- **Whisper and shout.** Registering a voice target is refused, so audio
  addressed to anything but the current channel or the local loopback is
  dropped.
- **Avatars, comments and textures.** Blob requests are refused, so a client
  shows none of them.
- **Registration and the user list.** There is no stored account model.
- **Encryption renegotiation.** A resynchronisation request is refused.

## Permissions

The permission bits advertised to a client cover traversal, entry, speech,
whisper and text. Channel and administration rights are deliberately withheld,
so a client does not render buttons whose only possible answer is a refusal.

Permissions are not a stored model. A query about a channel is answered from the
render that produced it, and an actual entry is validated again when it arrives.

## Refusals carry a reason

A refused message is answered with a denial naming its kind, except in two
cases the reference server also treats differently: a message over the rate
limit is dropped without an answer, since answering a flood is participating in
it, and an empty message is dropped because there is nothing to deliver.

Text is subject to four checks in order: the rate limit, emptiness, the
advertised length, and the presence of markup on a server that forbids it. A
server that forbids markup refuses it rather than rewriting it, because
stripping markup correctly means running a parser on client input.

Whether the connection may address that target at all is a separate question,
decided by the application. See [Refusals](../model/refusals.md).

## Idle accounting

Pings, permission queries, statistics requests, codec announcements, blob
requests and target registrations do not count as activity. A client sending
nothing else is idle, the same accounting the reference server applies.
