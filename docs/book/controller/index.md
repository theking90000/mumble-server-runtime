# Controller integration

The Controller integration puts the users of your application into voice chat,
without you writing any voice code.

You keep the part you already own: who your users are, what they are called,
and who should be able to hear whom. The runtime handles the Mumble protocol,
TLS, UDP voice, encryption, per-user channel views and audio routing.

Players connect with an unmodified Mumble client. There is no mod to install
and no resource pack.

## Who this is for

The main use case is a Minecraft server adding proximity or team voice chat, so
the examples are written with that in mind. Nothing in the library is tied to
Minecraft. The SDK has no Bukkit, Paper, Spigot or Minecraft dependency, and
works the same way in a Discord bot or a web backend.

You need to know Java. You do not need to know anything about audio, codecs,
network protocols or the Mumble specification.

## The two processes

An integration has two halves that talk to each other over gRPC:

```text
your application                the runtime
+---------------------+         +--------------------------+
| your plugin         |  gRPC   | mumble-controller-server |   TLS/UDP   Mumble
| + controller SDK    | <-----> | (Rust)                   | <---------> clients
+---------------------+         +--------------------------+
```

`mumble-controller-server` is a Rust process you run next to your game server.
It speaks Mumble to the players and gRPC to you.

The Spaces Java client (`be.theking90000.mumble:controller-spaces`) is the library you add to
your plugin. `ControllerSession` is almost all of it.

## Spaces, participants, sessions

Three concepts cover the entire API.

A **Space** is a place where people hear each other. Its name is any string you
choose: `lobby`, `team-red`, `arena-3`. A Space exists while at least one person
is in it. You never create or delete one.

A **participant** is one person your application is responsible for: a player,
a user, a bot. You give it an identifier that never changes, put it in a Space
and give it a display name. It exists for as long as you say it does, whether
or not their Mumble client is currently connected.

A **session** is your application's connection to the runtime, and the owner of
the participants it registered. One `ControllerSession` per plugin instance is
the normal setup.

## You describe, you do not command

You never send an instruction such as "move Steve to the red team channel".
You describe the state that should hold:

```java
handle.setSpec(ParticipantSpec.builder(SpaceKey.of("team-red"), "Steve").build());
```

Steve belongs in `team-red` and is displayed as `Steve`. That is his complete
description. The runtime compares it against the current state and does
whatever is needed to close the gap.

Three consequences for your code:

- There is no move, rename, mute or kick call. One call replaces a
  participant's description, and changing `spaceKey` moves them.
- You do not retry. If the connection drops during an update, the SDK
  reconnects and sends the description as it stands at that moment, not a
  backlog of pending operations. States you have already moved past are
  skipped.
- You do not track whether you are in sync. Set the description whenever your
  own state changes, as often as your game logic requires.

## Where to go next

[Getting started](getting-started.md) has a working program in about thirty
lines. After that:

- [Spaces](spaces.md) covers naming Spaces, their lifetime, and reading who is
  in one.
- [Participants](participants.md) covers registering, moving, muting, removing
  and reacting to players.
- [Connecting a player to Mumble](joining.md) covers how a player gets into
  voice.
- [A Minecraft plugin](minecraft.md) is a complete Bukkit plugin using all of
  it.
- [Troubleshooting](troubleshooting.md) lists the common failures.

The pages prefixed with **Reference** describe the protocol model, the
lifecycle state machines and the Rust server internals. They are background
material, and none of it is required to build a working integration.

## Status

This integration is pre-1.0 and still changing. The Java artifact is not yet
published to a public repository, and details may change between versions. The
protocol contract is versioned (`mumble.controller.v1`) and its compiled
descriptor digest is pinned in CI, so wire compatibility cannot break silently.
