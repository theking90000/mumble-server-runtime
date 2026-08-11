# Bukkit example plugin

A buildable version of the plugin described in
[`A Minecraft plugin`](../../../docs/book/controller/minecraft.md). It opens
one controller session, registers a participant per online player, keys the
Space on the player's world, sends a copyable `mumble://` join link, and releases
the participant on quit. A `/voice` command and a live sidebar expose the state the
SDK reports.

It exists to prove the shading recipe and the Java 8 target, not to be deployed
as-is. The SDK has no Minecraft dependency, and nothing here is required by it.

## Building

```sh
cd control-plane/reference/bukkit-spaces
../../gradlew build
```

The relocated, self-contained jar lands in
`build/libs/voice-example-plugin-0.1.0-SNAPSHOT.jar`.

This is a separate Gradle build, not a module of `mumble-controller`. The SDK
dependency is substituted onto `:implementations:spaces:clients:controller-spaces` of the neighbouring build, so the
example always compiles against the sources in this repository. Keeping it out
of the SDK build also keeps `./gradlew check` there free of the Spigot
repository, which is why CI does not build the example.

## Differences from the book page

- The package is `be.theking90000.mumble.bukkit` rather than
  `com.example.voice`, and the shading relocations follow it.
- `com.google.gson` and `com.google.thirdparty` are relocated in addition to the
  four packages the book lists. The Spigot server jar ships its own Gson.
- `mergeServiceFiles()` is required, together with `DuplicatesStrategy.INCLUDE`.
  gRPC resolves its name resolvers and load balancers through `META-INF/services`.
  Relocation without the merge leaves those descriptors pointing at classes that
  no longer exist under those names, and the default duplicates strategy drops
  the descriptors of `DnsNameResolverProvider` and `PickFirstLoadBalancerProvider`
  before the transformer runs.
- The join link uses `SUGGEST_COMMAND`, not `OPEN_URL`. The client follows
  `OPEN_URL` for `http` and `https` only and refuses any other scheme, so a
  `mumble://` address cannot be opened from chat. A click places the address in
  the chat box instead, where it can be selected and copied. The link text is
  the address itself rather than a label.
- Every SDK notification is traced, a `/voice` command reads the session back,
  and a sidebar renders it live. None of that is in the book page.

## Commands

`/voice` (alias `/vc`). `mute`, `unmute` and `list` require
`voiceexample.admin`, granted to operators by default.

| Subcommand | Effect |
| --- | --- |
| `/voice` or `/voice link` | Reprints the join link from `connectionCredential()`. |
| `/voice status` | Session state and controller id; for a player, handle state, applied Space, self mute and deaf, and the accepted, applied and generation revisions. |
| `/voice space [key]` | `fetchSpace` on the given key, or on the caller's own world Space. Lists the members the runtime reports. |
| `/voice board` | Toggles the sidebar. |
| `/voice list` | Every tracked participant with its handle state and Mumble connection. |
| `/voice mute <player>` | Sets `serverMute` in the player's spec. |
| `/voice unmute <player>` | Clears it. |

Administrative mutes are held in the plugin and reapplied by `specFor`, so a
world change does not silently clear one.

## Sidebar

Six rows: session state, handle state, Mumble connection, Space key, member
count of that Space, and microphone state. Yellow marks a value the runtime has
not applied yet, such as a Space key that differs from the desired one.

Values are read from the session cache by a repeating task on the server thread,
never pushed from an SDK callback. Each row is a team whose suffix carries the
value, because rewriting a score entry once per second makes the client flicker
and rewriting a team suffix does not. Spigot 1.8 caps a prefix and a suffix at
16 characters each, so longer Space keys are clipped.

Showing the sidebar replaces whatever scoreboard the player carried. Set
`scoreboard: false` to leave it to `/voice board`.

## Logging

`VoiceTrace` implements `ControllerSessionListener`, `ParticipantListener` and
`SpaceListener`, and logs each notification. Callbacks arrive on the session
callback executor, so that class touches no Bukkit API.

Always logged:

- session state transitions, with `RECONNECTING` as a warning and `FAILED` as an
  error;
- ownership losses, which require the player to rejoin;
- participant statuses carrying an application error;
- failed `setSpec` and `unregister` futures.

Logged only when `debug` is set:

- registrations, world moves with the accepted and applied spec revisions,
  unregistrations, and join-link delivery;
- participant handle transitions and full participant statuses;
- Space snapshots, with incarnation prefix, revision and member list.

The Mumble join token is a bearer credential. Its arrival is logged, its value
never is.

## Configuration

`config.yml` is copied into the plugin data folder on first enable.

| Key | Meaning |
| --- | --- |
| `controller-endpoint` | Address of the controller server. `https://` enables TLS. |
| `public-host` | Host written into the join link. Must resolve from the player's machine. |
| `controller-id` | Declarative controller identity. Shared by every server that shares its desired state. |
| `debug` | Traces every participant and Space notification to the console. |
| `log-join-link` | Also writes the join link to the server log. It carries a bearer credential, so this puts a password on disk. Off by default. |
| `scoreboard` | Shows the sidebar as soon as a player joins. |
| `scoreboard-refresh-ticks` | Sidebar refresh period. 20 ticks is one second. |

## Requirements

- Spigot 1.8.8 or newer, on Java 8 or newer.
- A running controller server; see
  [`The controller server`](../../../docs/book/controller/server.md).
