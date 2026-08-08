package be.theking90000.mumble.bukkit;

import be.theking90000.mumble.controller.ControllerId;
import be.theking90000.mumble.controller.ControllerSession;
import be.theking90000.mumble.controller.MumbleJoinToken;
import be.theking90000.mumble.controller.ParticipantHandle;
import be.theking90000.mumble.controller.ParticipantId;
import be.theking90000.mumble.controller.ParticipantSpec;
import be.theking90000.mumble.controller.SpaceKey;
import net.md_5.bungee.api.chat.ClickEvent;
import net.md_5.bungee.api.chat.ComponentBuilder;
import net.md_5.bungee.api.chat.HoverEvent;
import net.md_5.bungee.api.chat.TextComponent;
import org.bukkit.Bukkit;
import org.bukkit.ChatColor;
import org.bukkit.entity.Player;
import org.bukkit.event.EventHandler;
import org.bukkit.event.Listener;
import org.bukkit.event.player.PlayerChangedWorldEvent;
import org.bukkit.event.player.PlayerJoinEvent;
import org.bukkit.event.player.PlayerQuitEvent;
import org.bukkit.plugin.java.JavaPlugin;

import java.net.URI;
import java.util.Collections;
import java.util.Map;
import java.util.Set;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.CopyOnWriteArraySet;
import java.util.concurrent.TimeUnit;

/**
 * A Bukkit plugin that mirrors the server's players into one controller session.
 *
 * <p>One participant per online player, one Space per world. The SDK itself has no
 * Minecraft dependency; this class is only the adapter between Bukkit events and
 * the controller's desired state.</p>
 *
 * <p>Session lifecycle, ownership losses and application errors are always logged.
 * The per-player and per-Space traces are gated behind the {@code debug} key of
 * {@code config.yml}, and are emitted at {@code INFO} because a Bukkit console
 * discards anything below it.</p>
 */
public final class VoicePlugin extends JavaPlugin implements Listener {

    private final Map<UUID, ParticipantHandle> handles = new ConcurrentHashMap<UUID, ParticipantHandle>();
    private final Set<UUID> serverMuted = new CopyOnWriteArraySet<UUID>();
    private final VoiceTrace trace = new VoiceTrace(this);
    private VoiceScoreboard scoreboard;
    private ControllerSession session;
    private String publicHost;
    private boolean debug;
    private boolean logJoinLink;
    private boolean scoreboardByDefault;

    @Override
    public void onEnable() {
        saveDefaultConfig();
        publicHost = getConfig().getString("public-host", "127.0.0.1");
        debug = getConfig().getBoolean("debug", false);
        logJoinLink = getConfig().getBoolean("log-join-link", false);
        scoreboardByDefault = getConfig().getBoolean("scoreboard", true);
        int refreshTicks = Math.max(1, getConfig().getInt("scoreboard-refresh-ticks", 20));
        String controllerId = getConfig().getString("controller-id", "example-voice");
        String endpoint = getConfig().getString("controller-endpoint", "http://127.0.0.1:4000");

        getLogger().info("Controller " + controllerId + " at " + endpoint
                + ", join links pointing at " + publicHost
                + (debug ? ", debug traces on" : ""));

        session = ControllerSession.builder(ControllerId.of(controllerId), URI.create(endpoint))
                .build();
        session.addSessionListener(trace);
        session.addSpaceListener(trace);

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

        scoreboard = new VoiceScoreboard(this);
        scoreboard.start(refreshTicks);
        VoiceCommand voiceCommand = new VoiceCommand(this);
        getCommand("voice").setExecutor(voiceCommand);
        getCommand("voice").setTabCompleter(voiceCommand);
        getServer().getPluginManager().registerEvents(this, this);
    }

    @Override
    public void onDisable() {
        if (scoreboard != null) {
            scoreboard.stop();
        }
        if (session == null) {
            return;
        }
        debug("Stopping session with " + handles.size() + " tracked participant(s)");
        try {
            session.stop().get(5, TimeUnit.SECONDS);
            debug("Session stopped cleanly");
        } catch (Exception failure) {
            getLogger().warning("Voice chat did not shut down cleanly: " + failure);
        }
    }

    @EventHandler
    public void onJoin(PlayerJoinEvent event) {
        Player player = event.getPlayer();
        ParticipantSpec spec = specFor(player);
        debug("Registering " + player.getName() + " (" + player.getUniqueId() + ") in "
                + spec.spaceKey());

        ParticipantHandle handle = session.registerParticipant(
                ParticipantId.of(player.getUniqueId().toString()),
                spec);
        handle.addListener(trace);
        handles.put(player.getUniqueId(), handle);
        if (scoreboardByDefault) {
            scoreboard.show(player);
        }

        handle.whenMumbleJoinTokenAvailable().thenAccept(token -> {
            // Back to the server thread before touching the player.
            Bukkit.getScheduler().runTask(this, () -> sendJoinLink(player, token));
        });
    }

    @EventHandler
    public void onChangedWorld(PlayerChangedWorldEvent event) {
        Player player = event.getPlayer();
        ParticipantHandle handle = handles.get(player.getUniqueId());
        if (handle == null) {
            debug("No handle for " + player.getName() + " on world change, ignoring");
            return;
        }
        ParticipantSpec spec = specFor(player);
        debug("Moving " + player.getName() + " from " + event.getFrom().getName()
                + " to " + spec.spaceKey());
        handle.setSpec(spec).whenComplete((accepted, failure) -> {
            if (failure != null) {
                getLogger().warning("Move of " + player.getName() + " to " + spec.spaceKey()
                        + " failed: " + failure);
            } else {
                debug("Move of " + player.getName() + " accepted at spec revision "
                        + Long.toUnsignedString(accepted.acceptedSpecRevision())
                        + ", applied " + Long.toUnsignedString(accepted.appliedSpecRevision())
                        + ", generation " + Long.toUnsignedString(accepted.publishedGeneration()));
            }
        });
    }

    @EventHandler
    public void onQuit(PlayerQuitEvent event) {
        Player player = event.getPlayer();
        scoreboard.forget(player.getUniqueId());
        serverMuted.remove(player.getUniqueId());
        ParticipantHandle handle = handles.remove(player.getUniqueId());
        if (handle == null) {
            debug("No handle for " + player.getName() + " on quit, ignoring");
            return;
        }
        debug("Unregistering " + player.getName() + " from state " + handle.state());
        handle.unregister().whenComplete((ignored, failure) -> {
            if (failure != null) {
                getLogger().warning("Unregistration of " + player.getName() + " failed: " + failure);
            } else {
                debug("Unregistered " + player.getName());
            }
        });
    }

    /**
     * Sends the join URL as its own chat line.
     *
     * <p>The client only follows {@code OPEN_URL} for {@code http} and {@code https},
     * and refuses any other scheme, so a {@code mumble://} address cannot be opened
     * from chat. The line uses {@code SUGGEST_COMMAND} instead: a click places the
     * address in the chat box, where it can be selected and copied.</p>
     *
     * @param player recipient, checked for presence
     * @param token current credential for that player
     */
    void sendJoinLink(Player player, MumbleJoinToken token) {
        if (!player.isOnline()) {
            debug("Dropped join link for " + player.getName() + ", already offline");
            return;
        }
        String url = joinLink(player, token);
        player.sendMessage(ChatColor.AQUA + "Voice chat, copy this address into Mumble:");

        TextComponent link = new TextComponent(url);
        link.setColor(net.md_5.bungee.api.ChatColor.WHITE);
        link.setUnderlined(Boolean.TRUE);
        link.setClickEvent(new ClickEvent(ClickEvent.Action.SUGGEST_COMMAND, url));
        link.setHoverEvent(new HoverEvent(
                HoverEvent.Action.SHOW_TEXT,
                new ComponentBuilder("Click to put the address in the chat box, then copy it")
                        .create()));
        player.spigot().sendMessage(link);

        if (logJoinLink) {
            // The token is a bearer credential and this writes it to disk with the
            // rest of the server log. Off by default.
            getLogger().info("Join link for " + player.getName() + ": " + url);
        } else {
            debug("Sent join link to " + player.getName());
        }
    }

    String joinLink(Player player, MumbleJoinToken token) {
        return "mumble://" + player.getName() + ":" + token.value() + "@" + publicHost;
    }

    /**
     * Builds the desired specification of a player from the world they stand in and
     * the administrative mute set. Every call site goes through this method, so a
     * world change cannot silently drop a mute applied by an operator.
     *
     * @param player player to describe
     * @return complete desired specification
     */
    ParticipantSpec specFor(Player player) {
        return ParticipantSpec.builder(
                        SpaceKey.of("world-" + player.getWorld().getName()),
                        player.getName())
                .serverMute(serverMuted.contains(player.getUniqueId()))
                .build();
    }

    /**
     * Replaces the administrative mute of a player and pushes the new specification.
     *
     * @param player player to mute or unmute
     * @param muted requested state
     * @return true if the state changed, false if it already held
     */
    boolean setServerMuted(Player player, boolean muted) {
        boolean changed = muted
                ? serverMuted.add(player.getUniqueId())
                : serverMuted.remove(player.getUniqueId());
        if (!changed) {
            return false;
        }
        ParticipantHandle handle = handles.get(player.getUniqueId());
        if (handle != null) {
            handle.setSpec(specFor(player));
        }
        return true;
    }

    ControllerSession session() {
        return session;
    }

    VoiceScoreboard scoreboard() {
        return scoreboard;
    }

    ParticipantHandle handleOf(UUID playerId) {
        return handles.get(playerId);
    }

    Map<UUID, ParticipantHandle> handles() {
        return Collections.unmodifiableMap(handles);
    }

    boolean debugEnabled() {
        return debug;
    }

    void debug(String message) {
        if (debug) {
            getLogger().info("[debug] " + message);
        }
    }
}
