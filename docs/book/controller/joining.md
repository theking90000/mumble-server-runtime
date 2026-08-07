# Connecting a player to Mumble

Registering a participant does not put anyone in voice chat by itself. The
player still has to connect a Mumble client, and this page covers how.

## The join token

Along with ownership of a participant, the runtime issues a **Mumble join
token**: a password, specific to that participant, that lets an unmodified
Mumble client connect as them.

```java
handle.whenMumbleJoinTokenAvailable()
        .thenAccept(token -> sendJoinLink(player, token.value()));
```

Or read it directly, when the participant is already owned:

```java
Optional<MumbleJoinToken> token = handle.mumbleJoinToken();
```

The token is available from the moment ownership is granted, normally a few
milliseconds after `registerParticipant`.

## The join URL

The simplest way to connect a player is a `mumble://` link. Mumble registers
the scheme when it installs, so opening the link launches the client and
connects it:

```java
String url = "mumble://" + player.getName() + ":" + token.value() + "@voice.example.com";
```

Tokens are URL-safe base64, so nothing needs escaping.

The username in the URL is **ignored**. Only the password identifies the
participant. Put the player's name there anyway: Mumble displays it while
connecting, and it makes the link readable. The name other players see is the
`displayName` from the participant's spec.

In Minecraft, send it as a clickable chat component:

```java
TextComponent link = new TextComponent("Click here to join voice chat");
link.setClickEvent(new ClickEvent(ClickEvent.Action.OPEN_URL, url));
player.spigot().sendMessage(link);
```

The full plugin version is in [A Minecraft plugin](minecraft.md).

## Treat it as a password

The join token is a bearer credential. Anyone holding it can connect as that
participant and speak as them.

- Never log it, and never write it to a plaintext file.
- Never show it to another player. Send it only to its own participant, over a
  private channel.
- It grants no control over your session. A player who leaks their token cannot
  move participants, mute anyone or read Spaces. The damage is limited to
  impersonating that one participant.

`MumbleJoinToken.toString()` is redacted deliberately, so an accidental
`log.info("token: " + token)` prints nothing useful. Call `.value()` only where
you actually send the credential.

## Rotation

A token can change. It survives a reconnection, but acquiring the same
participant afresh issues a new one and invalidates the old.

Sending the token once at login is enough for most integrations. When a link
stays visible, or players can request it again, listen for rotations:

```java
handle.addListener(new ParticipantListener() {
    @Override
    public void onMumbleJoinTokenChanged(ParticipantHandle participant, MumbleJoinToken token) {
        updateStoredLink(participant.participantId(), token.value());
    }
});
```

Revoking or unregistering a participant invalidates its token immediately.

## Which address players connect to

Not the gRPC endpoint. Your Java code connects to the controller port (`4000`
by default, normally on localhost). Players connect to the Mumble port (`64738`
by default) on your server's public hostname.

Build the player-facing URL from that public hostname, not from the URI passed
to `ControllerSession.builder`.

## What the player sees

A connecting player arrives directly in the Space named by their spec, under
their display name, hearing exactly the participants the runtime decided they
should hear. There is no server-wide channel tree to browse and no way to walk
into another Space, because each connection receives its own view.

Moving a player between Spaces from your code is invisible in their client
beyond the change in who they can hear. They do not reconnect and they click
nothing.
