package be.theking90000.mumble.bukkit;

import be.theking90000.mumble.controller.core.ControllerSessionState;
import be.theking90000.mumble.controller.core.ConnectionCredential;
import be.theking90000.mumble.controller.core.ParticipantHandleState;
import be.theking90000.mumble.controller.spaces.ControllerSession;
import be.theking90000.mumble.controller.spaces.ControllerSessionListener;
import be.theking90000.mumble.controller.spaces.ParticipantHandle;
import be.theking90000.mumble.controller.spaces.ParticipantListener;
import be.theking90000.mumble.controller.spaces.ParticipantStatus;
import be.theking90000.mumble.controller.spaces.SpaceIncarnation;
import be.theking90000.mumble.controller.spaces.SpaceKey;
import be.theking90000.mumble.controller.spaces.SpaceListener;
import be.theking90000.mumble.controller.spaces.SpaceParticipant;
import be.theking90000.mumble.controller.spaces.SpaceSnapshot;

/**
 * Logs every notification the SDK delivers.
 *
 * <p>Callbacks arrive on the session callback executor, never on the server thread,
 * so nothing here may touch the Bukkit API. Logging is the only thing this class
 * does.</p>
 */
final class VoiceTrace implements ControllerSessionListener, ParticipantListener, SpaceListener {

    private final VoicePlugin plugin;

    VoiceTrace(VoicePlugin plugin) {
        this.plugin = plugin;
    }

    @Override
    public void onStateChanged(
            ControllerSession session,
            ControllerSessionState previous,
            ControllerSessionState current) {
        String message = "Session " + previous + " -> " + current;
        if (current == ControllerSessionState.FAILED) {
            plugin.getLogger().severe(message + "; voice chat is down until the plugin is reloaded");
        } else if (current == ControllerSessionState.RECONNECTING) {
            plugin.getLogger().warning(message + "; commands are queued until reconciliation completes");
        } else {
            plugin.getLogger().info(message);
        }
    }

    @Override
    public void onStateChanged(
            ParticipantHandle participant,
            ParticipantHandleState previous,
            ParticipantHandleState current) {
        plugin.debug("Participant " + name(participant) + " " + previous + " -> " + current);
    }

    @Override
    public void onStatusChanged(ParticipantHandle participant, ParticipantStatus status) {
        if (status.applicationError().isPresent()) {
            plugin.getLogger().warning("Participant " + name(participant) + " rejected: "
                    + status.applicationError().get());
            return;
        }
        plugin.debug("Participant " + name(participant)
                + " connected=" + status.connected()
                + " space=" + (status.appliedSpaceKey().isPresent()
                        ? status.appliedSpaceKey().get().value() : "none")
                + " selfMute=" + status.selfMute()
                + " selfDeaf=" + status.selfDeaf()
                + " accepted=" + Long.toUnsignedString(status.acceptedSpecRevision())
                + " applied=" + Long.toUnsignedString(status.appliedSpecRevision())
                + " generation=" + Long.toUnsignedString(status.publishedGeneration()));
    }

    @Override
    public void onConnectionCredentialChanged(ParticipantHandle participant, ConnectionCredential token) {
        // The token is a bearer credential. Log that one arrived, never its value.
        plugin.debug("Join token issued for " + name(participant));
    }

    @Override
    public void onOwnershipLost(ParticipantHandle participant, String reason) {
        plugin.getLogger().warning("Lost ownership of " + name(participant) + ": " + reason
                + "; the player must rejoin to get voice back");
    }

    @Override
    public void onSpaceUpdated(ControllerSession session, SpaceSnapshot snapshot) {
        if (!plugin.debugEnabled()) {
            return;
        }
        StringBuilder members = new StringBuilder();
        for (SpaceParticipant participant : snapshot.participants()) {
            if (members.length() > 0) {
                members.append(", ");
            }
            members.append(participant.displayName());
            if (!participant.connected()) {
                members.append("(pending)");
            }
            if (participant.serverMute()) {
                members.append("(muted)");
            }
            if (participant.serverDeaf()) {
                members.append("(deafened)");
            }
        }
        plugin.debug("Space " + snapshot.spaceKey().value()
                + " [" + shortId(snapshot.incarnation()) + "]"
                + " revision " + Long.toUnsignedString(snapshot.spaceRevision())
                + ", generation " + Long.toUnsignedString(snapshot.publishedGeneration())
                + ", " + snapshot.participants().size() + " participant(s): " + members);
    }

    @Override
    public void onSpaceClosed(
            ControllerSession session,
            SpaceKey spaceKey,
            SpaceIncarnation incarnation,
            long finalRevision) {
        plugin.debug("Space " + spaceKey.value() + " [" + shortId(incarnation) + "] closed at revision "
                + Long.toUnsignedString(finalRevision));
    }

    private String name(ParticipantHandle participant) {
        return participant.desiredSpec().displayName();
    }

    /** Renders the leading bytes of an incarnation, enough to tell two of them apart in a log. */
    static String shortId(SpaceIncarnation incarnation) {
        byte[] value = incarnation.value();
        StringBuilder text = new StringBuilder();
        for (int index = 0; index < value.length && index < 4; index++) {
            text.append(String.format("%02x", value[index] & 0xff));
        }
        return text.toString();
    }
}
