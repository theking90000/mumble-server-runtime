package be.theking90000.mumble.bukkit;

import be.theking90000.mumble.controller.core.ControllerSessionState;
import be.theking90000.mumble.controller.core.ParticipantHandleState;
import be.theking90000.mumble.controller.spaces.ParticipantHandle;
import be.theking90000.mumble.controller.spaces.ParticipantSpec;
import be.theking90000.mumble.controller.spaces.ParticipantStatus;
import be.theking90000.mumble.controller.spaces.SpaceKey;
import be.theking90000.mumble.controller.spaces.SpaceSnapshot;
import org.bukkit.Bukkit;
import org.bukkit.ChatColor;
import org.bukkit.entity.Player;
import org.bukkit.scheduler.BukkitTask;
import org.bukkit.scoreboard.DisplaySlot;
import org.bukkit.scoreboard.Objective;
import org.bukkit.scoreboard.Scoreboard;
import org.bukkit.scoreboard.Team;

import java.util.HashMap;
import java.util.Iterator;
import java.util.Map;
import java.util.Optional;
import java.util.UUID;

/**
 * A sidebar showing the live voice state of one player.
 *
 * <p>Every value is read from the session cache on the server thread by a repeating
 * task, never pushed from an SDK callback. {@code ControllerSession#state()},
 * {@code ControllerSession#spaces()} and the handle accessors all return
 * point-in-time snapshots and are safe to call from anywhere.</p>
 *
 * <p>Lines are held by a team per row rather than by the score entries themselves.
 * Rewriting an entry means removing and re-adding it, which the client renders as a
 * flicker once per second; rewriting a team prefix does not. Spigot 1.8 caps a
 * prefix and a suffix at 16 characters each, hence the two-column layout.</p>
 */
final class VoiceScoreboard {

    private static final String[] LABELS = {"Session", "Handle", "Voice", "Space", "Members", "Mic"};
    private static final int SEGMENT_LIMIT = 16;

    private final VoicePlugin plugin;
    /** Server-thread confinement: created, mutated and read from the main thread only. */
    private final Map<UUID, PlayerBoard> boards = new HashMap<UUID, PlayerBoard>();
    private BukkitTask task;

    VoiceScoreboard(VoicePlugin plugin) {
        this.plugin = plugin;
    }

    void start(int refreshTicks) {
        task = Bukkit.getScheduler().runTaskTimer(plugin, new Runnable() {
            @Override
            public void run() {
                refresh();
            }
        }, refreshTicks, refreshTicks);
    }

    void stop() {
        if (task != null) {
            task.cancel();
            task = null;
        }
        boards.clear();
    }

    boolean isShown(UUID playerId) {
        return boards.containsKey(playerId);
    }

    /**
     * Shows the sidebar to a player, replacing whatever scoreboard they carried.
     *
     * @param player player to attach a fresh sidebar to
     */
    void show(Player player) {
        PlayerBoard board = new PlayerBoard();
        boards.put(player.getUniqueId(), board);
        player.setScoreboard(board.scoreboard);
        refresh();
    }

    /**
     * Hides the sidebar and returns the player to the server's main scoreboard.
     *
     * @param player player to detach
     */
    void hide(Player player) {
        if (boards.remove(player.getUniqueId()) != null) {
            player.setScoreboard(Bukkit.getScoreboardManager().getMainScoreboard());
        }
    }

    /**
     * Drops the state of a player who left, without touching their scoreboard.
     *
     * @param playerId player who disconnected
     */
    void forget(UUID playerId) {
        boards.remove(playerId);
    }

    private void refresh() {
        ControllerSessionState sessionState = plugin.session().state();
        Map<SpaceKey, SpaceSnapshot> spaces = plugin.session().spaces();
        Iterator<Map.Entry<UUID, PlayerBoard>> entries = boards.entrySet().iterator();
        while (entries.hasNext()) {
            Map.Entry<UUID, PlayerBoard> entry = entries.next();
            Player player = Bukkit.getPlayer(entry.getKey());
            if (player == null) {
                entries.remove();
                continue;
            }
            render(entry.getValue(), plugin.handleOf(entry.getKey()), sessionState, spaces);
        }
    }

    private void render(
            PlayerBoard board,
            ParticipantHandle handle,
            ControllerSessionState sessionState,
            Map<SpaceKey, SpaceSnapshot> spaces) {
        board.line(0, sessionColor(sessionState), sessionState.name());

        if (handle == null) {
            board.line(1, ChatColor.RED, "none");
            board.line(2, ChatColor.GRAY, "-");
            board.line(3, ChatColor.GRAY, "-");
            board.line(4, ChatColor.GRAY, "-");
            board.line(5, ChatColor.GRAY, "-");
            return;
        }

        ParticipantHandleState handleState = handle.state();
        board.line(1, handleColor(handleState), handleState.name());

        Optional<ParticipantStatus> status = handle.latestStatus();
        ParticipantSpec spec = handle.desiredSpec();
        boolean connected = status.isPresent() && status.get().connected();
        board.line(2, connected ? ChatColor.GREEN : ChatColor.YELLOW,
                connected ? "connected" : "not joined");

        SpaceKey space = spec.spaceKey();
        if (status.isPresent() && status.get().appliedSpaceKey().isPresent()) {
            space = status.get().appliedSpaceKey().get();
        }
        boolean applied = status.isPresent()
                && status.get().appliedSpaceKey().isPresent()
                && status.get().appliedSpaceKey().get().equals(spec.spaceKey());
        board.line(3, applied ? ChatColor.WHITE : ChatColor.YELLOW, space.value());

        SpaceSnapshot snapshot = spaces.get(space);
        board.line(4, ChatColor.WHITE,
                snapshot == null ? "unknown" : Integer.toString(snapshot.participants().size()));

        board.line(5, micColor(spec, status), micState(spec, status));
    }

    private static String micState(ParticipantSpec spec, Optional<ParticipantStatus> status) {
        if (spec.serverDeaf()) {
            // Kept short: a suffix is clipped at 16 characters, colour code included.
            return "server deaf";
        }
        if (spec.serverMute()) {
            return "server muted";
        }
        if (status.isPresent() && status.get().selfDeaf()) {
            return "deafened";
        }
        if (status.isPresent() && status.get().selfMute()) {
            return "muted";
        }
        return "open";
    }

    private static ChatColor micColor(ParticipantSpec spec, Optional<ParticipantStatus> status) {
        if (spec.serverMute() || spec.serverDeaf()) {
            return ChatColor.RED;
        }
        if (status.isPresent() && (status.get().selfMute() || status.get().selfDeaf())) {
            return ChatColor.YELLOW;
        }
        return ChatColor.GREEN;
    }

    private static ChatColor sessionColor(ControllerSessionState state) {
        switch (state) {
            case ACTIVE:
                return ChatColor.GREEN;
            case FAILED:
            case CLOSED:
                return ChatColor.RED;
            default:
                return ChatColor.YELLOW;
        }
    }

    private static ChatColor handleColor(ParticipantHandleState state) {
        switch (state) {
            case OWNED:
                return ChatColor.GREEN;
            case REVOKED:
            case CLOSED:
                return ChatColor.RED;
            default:
                return ChatColor.YELLOW;
        }
    }

    /** One player's sidebar, with a fixed row count so that no row is ever added or removed. */
    private static final class PlayerBoard {
        private final Scoreboard scoreboard;
        private final Team[] rows = new Team[LABELS.length];
        private final String[] rendered = new String[LABELS.length];

        PlayerBoard() {
            scoreboard = Bukkit.getScoreboardManager().getNewScoreboard();
            Objective objective = scoreboard.registerNewObjective("voice", "dummy");
            objective.setDisplayName(ChatColor.AQUA + "" + ChatColor.BOLD + "Voice");
            objective.setDisplaySlot(DisplaySlot.SIDEBAR);
            for (int row = 0; row < LABELS.length; row++) {
                // A colour code makes an entry that is unique per row and renders as
                // nothing; the reset keeps it from tinting the suffix that follows.
                String entry = ChatColor.values()[row].toString() + ChatColor.RESET;
                Team team = scoreboard.registerNewTeam("voice-row-" + row);
                team.addEntry(entry);
                team.setPrefix(clip(ChatColor.GRAY + LABELS[row] + " "));
                // Rows are drawn by descending score, so row zero has to score highest.
                objective.getScore(entry).setScore(LABELS.length - row);
                rows[row] = team;
            }
        }

        void line(int row, ChatColor color, String value) {
            String text = clip(color + value);
            if (!text.equals(rendered[row])) {
                rows[row].setSuffix(text);
                rendered[row] = text;
            }
        }

        private static String clip(String text) {
            return text.length() <= SEGMENT_LIMIT ? text : text.substring(0, SEGMENT_LIMIT);
        }
    }
}
