package be.theking90000.mumble.bukkit;

import be.theking90000.mumble.controller.core.ControllerException;
import be.theking90000.mumble.controller.core.ConnectionCredential;
import be.theking90000.mumble.controller.spaces.ParticipantHandle;
import be.theking90000.mumble.controller.spaces.ParticipantStatus;
import be.theking90000.mumble.controller.spaces.SpaceKey;
import be.theking90000.mumble.controller.spaces.SpaceParticipant;
import be.theking90000.mumble.controller.spaces.SpaceSnapshot;
import org.bukkit.Bukkit;
import org.bukkit.ChatColor;
import org.bukkit.command.Command;
import org.bukkit.command.CommandExecutor;
import org.bukkit.command.CommandSender;
import org.bukkit.command.TabCompleter;
import org.bukkit.entity.Player;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.UUID;

/**
 * Handler of {@code /voice}.
 *
 * <p>Read-only subcommands answer from the session cache, which is a local snapshot
 * and needs no round trip. {@code space} is the exception: it queries the runtime
 * and answers from a future, so its reply is scheduled back onto the server thread.</p>
 */
final class VoiceCommand implements CommandExecutor, TabCompleter {

    private static final String ADMIN_PERMISSION = "voiceexample.admin";
    private static final List<String> SUBCOMMANDS =
            Collections.unmodifiableList(Arrays.asList("link", "status", "space", "board", "list", "mute", "unmute"));

    private final VoicePlugin plugin;

    VoiceCommand(VoicePlugin plugin) {
        this.plugin = plugin;
    }

    @Override
    public boolean onCommand(CommandSender sender, Command command, String label, String[] args) {
        String subcommand = args.length == 0 ? "link" : args[0].toLowerCase();
        try {
            if ("link".equals(subcommand)) {
                return link(sender);
            }
            if ("status".equals(subcommand)) {
                return status(sender);
            }
            if ("space".equals(subcommand)) {
                return space(sender, args);
            }
            if ("board".equals(subcommand)) {
                return board(sender);
            }
            if ("list".equals(subcommand)) {
                return list(sender);
            }
            if ("mute".equals(subcommand)) {
                return mute(sender, args, true);
            }
            if ("unmute".equals(subcommand)) {
                return mute(sender, args, false);
            }
        } catch (ControllerException failure) {
            error(sender, "The controller session refused the request: " + failure.getMessage());
            return true;
        }
        return false;
    }

    private boolean link(CommandSender sender) {
        Player player = requirePlayer(sender);
        if (player == null) {
            return true;
        }
        ParticipantHandle handle = plugin.handleOf(player.getUniqueId());
        if (handle == null) {
            error(sender, "No voice participant is registered for you; rejoin the server.");
            return true;
        }
        Optional<ConnectionCredential> token = handle.connectionCredential();
        if (!token.isPresent()) {
            error(sender, "The runtime has not issued a join token yet; try again shortly.");
            return true;
        }
        plugin.sendJoinLink(player, token.get());
        return true;
    }

    private boolean status(CommandSender sender) {
        sender.sendMessage(ChatColor.AQUA + "Session " + ChatColor.WHITE
                + plugin.session().controllerId().value()
                + ChatColor.GRAY + " (" + plugin.session().state() + ")");
        sender.sendMessage(ChatColor.AQUA + "Participants " + ChatColor.WHITE
                + plugin.handles().size()
                + ChatColor.GRAY + ", observed spaces " + plugin.session().spaces().size());

        if (!(sender instanceof Player)) {
            return true;
        }
        ParticipantHandle handle = plugin.handleOf(((Player) sender).getUniqueId());
        if (handle == null) {
            error(sender, "No voice participant is registered for you.");
            return true;
        }
        sender.sendMessage(ChatColor.AQUA + "Handle " + ChatColor.WHITE + handle.state()
                + ChatColor.GRAY + ", desired space " + handle.desiredSpec().spaceKey().value()
                + (handle.desiredSpec().serverMute() ? ", server muted" : ""));
        Optional<ParticipantStatus> status = handle.latestStatus();
        if (!status.isPresent()) {
            sender.sendMessage(ChatColor.GRAY + "No status reported by the runtime yet.");
            return true;
        }
        ParticipantStatus current = status.get();
        sender.sendMessage(ChatColor.AQUA + "Mumble " + ChatColor.WHITE
                + (current.connected() ? "connected" : "not joined")
                + ChatColor.GRAY + ", applied space "
                + (current.appliedSpaceKey().isPresent()
                        ? current.appliedSpaceKey().get().value() : "none")
                + ", self mute " + current.selfMute()
                + ", self deaf " + current.selfDeaf());
        sender.sendMessage(ChatColor.GRAY + "Revisions: accepted "
                + Long.toUnsignedString(current.acceptedSpecRevision())
                + ", applied " + Long.toUnsignedString(current.appliedSpecRevision())
                + ", generation " + Long.toUnsignedString(current.publishedGeneration()));
        if (current.applicationError().isPresent()) {
            error(sender, "Runtime error: " + current.applicationError().get());
        }
        return true;
    }

    private boolean space(final CommandSender sender, String[] args) {
        final SpaceKey key;
        if (args.length >= 2) {
            key = SpaceKey.of(args[1]);
        } else {
            Player player = requirePlayer(sender);
            if (player == null) {
                return true;
            }
            key = plugin.specFor(player).spaceKey();
        }

        sender.sendMessage(ChatColor.GRAY + "Fetching " + key.value() + "...");
        plugin.session().fetchSpace(key).whenComplete((snapshot, failure) -> {
            // Futures complete on the SDK callback executor; messaging a sender is
            // Bukkit API and belongs on the server thread.
            Bukkit.getScheduler().runTask(plugin, () -> {
                if (failure != null) {
                    error(sender, "Could not fetch " + key.value() + ": " + failure.getMessage());
                } else {
                    describe(sender, snapshot);
                }
            });
        });
        return true;
    }

    private void describe(CommandSender sender, SpaceSnapshot snapshot) {
        sender.sendMessage(ChatColor.AQUA + snapshot.spaceKey().value() + ChatColor.GRAY
                + " [" + VoiceTrace.shortId(snapshot.incarnation()) + "]"
                + " revision " + Long.toUnsignedString(snapshot.spaceRevision())
                + ", " + snapshot.participants().size() + " participant(s)");
        for (SpaceParticipant participant : snapshot.participants()) {
            StringBuilder flags = new StringBuilder();
            if (!participant.connected()) {
                flags.append(" pending");
            }
            if (participant.serverMute()) {
                flags.append(" server-muted");
            }
            if (participant.serverDeaf()) {
                flags.append(" server-deafened");
            }
            sender.sendMessage(ChatColor.WHITE + " - " + participant.displayName()
                    + ChatColor.GRAY + flags);
        }
    }

    private boolean board(CommandSender sender) {
        Player player = requirePlayer(sender);
        if (player == null) {
            return true;
        }
        if (plugin.scoreboard().isShown(player.getUniqueId())) {
            plugin.scoreboard().hide(player);
            sender.sendMessage(ChatColor.GRAY + "Voice sidebar hidden.");
        } else {
            plugin.scoreboard().show(player);
            sender.sendMessage(ChatColor.GRAY + "Voice sidebar shown.");
        }
        return true;
    }

    private boolean list(CommandSender sender) {
        if (!requireAdmin(sender)) {
            return true;
        }
        Map<UUID, ParticipantHandle> handles = plugin.handles();
        sender.sendMessage(ChatColor.AQUA + "Tracked participants: " + handles.size());
        for (ParticipantHandle handle : handles.values()) {
            Optional<ParticipantStatus> status = handle.latestStatus();
            sender.sendMessage(ChatColor.WHITE + " - " + handle.desiredSpec().displayName()
                    + ChatColor.GRAY + " " + handle.state()
                    + " in " + handle.desiredSpec().spaceKey().value()
                    + ", mumble " + (status.isPresent() && status.get().connected()
                            ? "connected" : "not joined"));
        }
        return true;
    }

    private boolean mute(CommandSender sender, String[] args, boolean muted) {
        if (!requireAdmin(sender)) {
            return true;
        }
        if (args.length < 2) {
            error(sender, "Usage: /voice " + (muted ? "mute" : "unmute") + " <player>");
            return true;
        }
        Player target = Bukkit.getPlayerExact(args[1]);
        if (target == null) {
            error(sender, "No player named " + args[1] + " is online.");
            return true;
        }
        plugin.setServerMuted(target, muted).whenComplete((changed, failure) -> {
            // Controller callbacks do not run on the Bukkit server thread.
            Bukkit.getScheduler().runTask(plugin, () -> {
                if (failure != null) {
                    error(sender, "Could not " + (muted ? "mute " : "unmute ")
                            + target.getName() + ": " + failure.getMessage());
                } else if (changed.booleanValue()) {
                    sender.sendMessage(ChatColor.GRAY + target.getName()
                            + (muted ? " is now server muted." : " is no longer server muted."));
                } else {
                    sender.sendMessage(ChatColor.GRAY + target.getName()
                            + " was already in that state.");
                }
            });
        });
        return true;
    }

    private Player requirePlayer(CommandSender sender) {
        if (sender instanceof Player) {
            return (Player) sender;
        }
        error(sender, "This subcommand needs a player. Use /voice space <key> from the console.");
        return null;
    }

    private boolean requireAdmin(CommandSender sender) {
        if (sender.hasPermission(ADMIN_PERMISSION)) {
            return true;
        }
        error(sender, "You do not have permission to do that.");
        return false;
    }

    private void error(CommandSender sender, String message) {
        sender.sendMessage(ChatColor.RED + message);
    }

    @Override
    public List<String> onTabComplete(CommandSender sender, Command command, String alias, String[] args) {
        if (args.length == 1) {
            return matching(SUBCOMMANDS, args[0]);
        }
        if (args.length == 2 && ("mute".equalsIgnoreCase(args[0]) || "unmute".equalsIgnoreCase(args[0]))) {
            List<String> names = new ArrayList<String>();
            for (Player player : Bukkit.getOnlinePlayers()) {
                names.add(player.getName());
            }
            return matching(names, args[1]);
        }
        if (args.length == 2 && "space".equalsIgnoreCase(args[0])) {
            List<String> keys = new ArrayList<String>();
            for (SpaceKey key : plugin.session().spaces().keySet()) {
                keys.add(key.value());
            }
            return matching(keys, args[1]);
        }
        return Collections.emptyList();
    }

    private static List<String> matching(List<String> candidates, String prefix) {
        List<String> matches = new ArrayList<String>();
        String lowered = prefix.toLowerCase();
        for (String candidate : candidates) {
            if (candidate.toLowerCase().startsWith(lowered)) {
                matches.add(candidate);
            }
        }
        return matches;
    }
}
