# A Minecraft plugin

The previous pages are plain Java and apply to any application. This page
assembles them into a working Bukkit plugin, which is the most common case.

None of this is required by the SDK, which has no Minecraft dependency. A
Discord bot or a web backend uses the same API.

## What the plugin does

- Opens one controller session when the server starts.
- Registers a participant when a player joins, and sends them a clickable join
  link.
- Moves participants between Spaces when players change world.
- Unregisters a participant when a player leaves.
- Closes the session cleanly on shutdown.

## The plugin

```java
package com.example.voice;

import be.theking90000.mumble.controller.*;
import net.md_5.bungee.api.chat.ClickEvent;
import net.md_5.bungee.api.chat.TextComponent;
import org.bukkit.Bukkit;
import org.bukkit.entity.Player;
import org.bukkit.event.EventHandler;
import org.bukkit.event.Listener;
import org.bukkit.event.player.PlayerChangedWorldEvent;
import org.bukkit.event.player.PlayerJoinEvent;
import org.bukkit.event.player.PlayerQuitEvent;
import org.bukkit.plugin.java.JavaPlugin;

import java.net.URI;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.TimeUnit;

public final class VoicePlugin extends JavaPlugin implements Listener {

    private final Map<UUID, ParticipantHandle> handles = new ConcurrentHashMap<UUID, ParticipantHandle>();
    private ControllerSession session;
    private String publicHost;

    @Override
    public void onEnable() {
        saveDefaultConfig();
        publicHost = getConfig().getString("public-host", "127.0.0.1");
        String endpoint = getConfig().getString("controller-endpoint", "http://127.0.0.1:4000");

        session = ControllerSession.builder(
                        ControllerId.of("example-voice"),
                        URI.create(endpoint))
                .build();

        // Do not block the server thread. Players who join before the session is
        // up are still registered correctly: their registration is part of the
        // opening snapshot.
        session.start().whenComplete((ignored, failure) -> {
            if (failure != null) {
                getLogger().severe("Voice chat unavailable: " + failure.getMessage());
            } else {
                getLogger().info("Voice chat connected.");
            }
        });

        getServer().getPluginManager().registerEvents(this, this);
    }

    @Override
    public void onDisable() {
        if (session == null) {
            return;
        }
        try {
            session.stop().get(5, TimeUnit.SECONDS);
        } catch (Exception failure) {
            getLogger().warning("Voice chat did not shut down cleanly: " + failure);
        }
    }

    @EventHandler
    public void onJoin(PlayerJoinEvent event) {
        Player player = event.getPlayer();
        ParticipantHandle handle = session.registerParticipant(
                ParticipantId.of(player.getUniqueId().toString()),
                specFor(player));
        handles.put(player.getUniqueId(), handle);

        handle.whenMumbleJoinTokenAvailable().thenAccept(token -> {
            String url = "mumble://" + player.getName() + ":" + token.value() + "@" + publicHost;
            // Back to the server thread before touching the player.
            Bukkit.getScheduler().runTask(this, () -> {
                if (player.isOnline()) {
                    TextComponent link = new TextComponent("Click here to join voice chat");
                    link.setClickEvent(new ClickEvent(ClickEvent.Action.OPEN_URL, url));
                    player.spigot().sendMessage(link);
                }
            });
        });
    }

    @EventHandler
    public void onChangedWorld(PlayerChangedWorldEvent event) {
        ParticipantHandle handle = handles.get(event.getPlayer().getUniqueId());
        if (handle != null) {
            handle.setSpec(specFor(event.getPlayer()));
        }
    }

    @EventHandler
    public void onQuit(PlayerQuitEvent event) {
        ParticipantHandle handle = handles.remove(event.getPlayer().getUniqueId());
        if (handle != null) {
            handle.unregister();
        }
    }

    private ParticipantSpec specFor(Player player) {
        return ParticipantSpec.builder(
                        SpaceKey.of("world-" + player.getWorld().getName()),
                        player.getName())
                .build();
    }
}
```

With `config.yml`:

```yaml
controller-endpoint: "http://127.0.0.1:4000"
public-host: "voice.example.com"
```

The plugin above is a working integration. Most of what you would add next
belongs in `specFor`.

## Threading

Two rules.

**Never block the server thread.** `start()`, `setSpec()`, `unregister()` and
`observeSpace()` all return futures, and none of them block. The only call to
wait on is `stop()` in `onDisable`, with a timeout.

**Callbacks are not on the server thread.** Listener callbacks and future
continuations run on the SDK's callback executor. Reading a `ParticipantStatus`
there is fine. Touching a `Player`, the world or any Bukkit API is not: return
to the server thread with `Bukkit.getScheduler().runTask(plugin, ...)`, as
`onJoin` does above.

Your own executor can be passed to the builder when callbacks need to run
somewhere specific:

```java
ControllerSession.builder(id, endpoint)
        .callbackExecutor(myExecutor)
        .build();
```

Do not pass an executor that runs tasks on the server thread. Callbacks are
serialized, so one blocked callback stops the session from processing anything
else.

## Shading

The SDK pulls in gRPC and Protobuf. When another plugin on the same server
ships different versions, whichever loads first wins and one of the two breaks.
Shade both into your jar and relocate them.

With the Gradle Shadow plugin:

```kotlin
plugins {
    id("com.gradleup.shadow") version "8.3.5"
}

dependencies {
    implementation("be.theking90000.mumble:controller-spaces:0.1.0")
}

tasks.shadowJar {
    relocate("io.grpc", "com.example.voice.libs.grpc")
    relocate("com.google.protobuf", "com.example.voice.libs.protobuf")
    relocate("com.google.common", "com.example.voice.libs.guava")
    relocate("io.perfmark", "com.example.voice.libs.perfmark")
}
```

Do not enable `minimize()`. gRPC resolves transports and codecs through service
loaders and reflection, so minimisation removes classes that nothing references
statically.

## Going further

**Proximity chat.** Compute the Space key from a coarse grid over the world and
call `setSpec` when a player crosses a cell boundary, rather than every tick.

**Team chat.** Key on the team name. Switching teams is one `setSpec`.

**Admin mute.** Set `serverMute(true)` in the spec. To display an indicator for
players who muted *themselves*, read `selfMute()` from `ParticipantStatus` in
`onStatusChanged`.

**A `/voice` command.** Re-send the join link by reading
`handle.mumbleJoinToken()`, or list a Space with `session.fetchSpace(...)`.

**Cross-server voice.** Two Minecraft servers pointing at the same controller
server and using the same key convention put their players in the same Space.
Each keeps ownership of its own players.
