# Participants

A participant is one person your application is responsible for. This page
covers registering one, changing it, watching it and removing it.

## Identifier and display name

A participant has two names, used for different things:

```java
ParticipantId.of("069a79f4-44e9-4726-a5be-fca90e38aaf5")   // your identifier
ParticipantSpec.builder(SpaceKey.of("lobby"), "Notch")      // shown to players
```

`ParticipantId` is the key your own code uses. It must never change for the
same person, so use a Minecraft UUID, a database primary key or an account
identifier. It is never displayed to anyone.

The display name is what other players read in their Mumble client. It can
change whenever you want and does not have to be unique.

## Registering

```java
ParticipantHandle handle = session.registerParticipant(
        ParticipantId.of(player.getUniqueId().toString()),
        ParticipantSpec.builder(SpaceKey.of("lobby"), player.getName()).build());
```

`registerParticipant` returns a `ParticipantHandle` immediately, before the
runtime has confirmed anything. Every later operation on that participant goes
through the handle, so store it in a map keyed by your own player identifier.

Registering neither blocks nor fails because the connection is down. When the
session is not connected yet, the registration is part of the snapshot sent as
soon as it is.

Calling it twice for the same identifier while the first registration is still
alive throws `IllegalStateException`. Either unregister first, or retrieve the
existing handle with `session.participant(id)`.

## The specification

`ParticipantSpec` is the complete description of a participant, in four fields:

| Field | Meaning |
|---|---|
| `spaceKey` | Which Space this participant belongs to |
| `displayName` | Name shown in other players' Mumble clients |
| `serverMute` | This participant may not transmit |
| `serverDeaf` | This participant may not receive |

```java
ParticipantSpec spec = ParticipantSpec.builder(SpaceKey.of("team-red"), "Notch")
        .serverMute(true)
        .build();
```

`serverMute` and `serverDeaf` are decisions you impose on the player. They are
not the mute and deafen buttons in the player's own Mumble client, which you
can read but not set. See [Reacting to players](#reacting-to-players).

## Changing a participant

A single call replaces the whole description:

```java
handle.setSpec(ParticipantSpec.builder(SpaceKey.of("team-blue"), "Notch").build());
```

Changing `spaceKey` moves the participant. There is no separate move, rename or
mute operation: build the description that should hold and set it.

Since the description is replaced as a whole, build it from your own current
state rather than from a partly remembered earlier spec. Computing the whole
spec in one place is usually simplest:

```java
private ParticipantSpec specFor(Player player) {
    return ParticipantSpec.builder(spaceOf(player), player.getName())
            .serverMute(isSilenced(player))
            .build();
}

// wherever your game state changes
handle.setSpec(specFor(player));
```

Call it as often as you need. Setting the same value twice is harmless, and
rapid successive changes are coalesced so that only the newest description is
sent.

### Knowing when a change took effect

`setSpec` returns a future that you can usually ignore. When you do need it, it
completes with an `AcceptedRevision` carrying three separate milestones:

```java
handle.setSpec(spec).thenAccept(revision -> {
    revision.acceptedSpecRevision();   // the runtime took your description
    revision.appliedSpecRevision();    // it was used to compute a new layout
    revision.publishedGeneration();    // that layout reached Mumble clients
});
```

They are separate because a successful call does not prove that any client has
updated yet. Most application logic can ignore all three. Use them for
diagnostics, or when something else has to wait until a move is visible.

## Removing a participant

```java
handle.unregister();
```

The participant leaves its Space and its Mumble connection is closed. Do this
when a player leaves your server.

Unregistering is final for that handle. If the same player returns, call
`registerParticipant` again and use the new handle it returns.

## Reacting to players

The runtime reports what participants are actually doing through
`ParticipantListener`. Every method has a default implementation, so override
only the ones you need:

```java
handle.addListener(new ParticipantListener() {
    @Override
    public void onStatusChanged(ParticipantHandle participant, ParticipantStatus status) {
        if (status.mumbleConnected()) {
            // their Mumble client is connected
        }
        if (status.selfMute()) {
            // they muted themselves in their own client
        }
    }

    @Override
    public void onOwnershipLost(ParticipantHandle participant, String reason) {
        // another application took this participant, or the lease expired
    }
});
```

`ParticipantStatus` reports observed reality, where the spec states your
intent:

| Method | Reports |
|---|---|
| `mumbleConnected()` | Whether a Mumble client is currently connected |
| `appliedSpaceKey()` | Which Space they are actually in |
| `selfMute()`, `selfDeaf()` | What the player set in their own client |
| `applicationError()` | Why a description could not be honoured, when it could not |

Reading `selfMute()` is how you show a muted icon above a player's head, or
refuse a voice-only feature to a player who has muted themselves.

> **Callbacks do not run on your main thread.** They arrive on the session's
> callback executor, serialized in order. Never block them, and return to your
> game's own thread before touching game state. In Bukkit that means
> `Bukkit.getScheduler().runTask(plugin, ...)`.

## Handle states

`handle.state()` reports where a participant stands in its ownership
lifecycle.

| State | Meaning | What to do |
|---|---|---|
| `ACQUIRING` | Registered, waiting for the runtime | Nothing, this is normal |
| `OWNED` | Yours, working | Nothing |
| `SUSPENDED` | Connection lost, still yours | Nothing, it recovers by itself |
| `REVOKED` | Permanently lost | Register again if the player is still online |
| `CLOSED` | You unregistered it | Nothing |

`SUSPENDED` does not mean the player was disconnected from voice. Ownership is
held by a server-side lease that outlives the gRPC connection, so a brief
network problem or a controller restart leaves players talking. Your desired
state is kept and re-sent on reconnect.

`REVOKED` is permanent. It happens when another application registers the same
`ParticipantId`, or when the lease expires because you were gone too long. The
handle is dead, and a new registration produces a new one.
