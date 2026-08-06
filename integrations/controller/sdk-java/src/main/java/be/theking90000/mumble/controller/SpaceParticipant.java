package be.theking90000.mumble.controller;

import java.util.Objects;

/** Participant projection included in a full space snapshot. */
public final class SpaceParticipant {
    private final ParticipantId participantId;
    private final String displayName;
    private final boolean serverMute;
    private final boolean serverDeaf;
    private final boolean mumbleConnected;

    SpaceParticipant(
            ParticipantId participantId,
            String displayName,
            boolean serverMute,
            boolean serverDeaf,
            boolean mumbleConnected) {
        this.participantId = Objects.requireNonNull(participantId, "participantId");
        this.displayName = Objects.requireNonNull(displayName, "displayName");
        this.serverMute = serverMute;
        this.serverDeaf = serverDeaf;
        this.mumbleConnected = mumbleConnected;
    }

    /**
     * Returns the participant's stable controller identity.
     *
     * @return participant identity
     */
    public ParticipantId participantId() {
        return participantId;
    }

    /**
     * Returns the display name materialized in this snapshot.
     *
     * @return display name
     */
    public String displayName() {
        return displayName;
    }

    /**
     * Returns the server-mute state materialized in this snapshot.
     *
     * @return materialized server-mute state
     */
    public boolean serverMute() {
        return serverMute;
    }

    /**
     * Returns the server-deaf state materialized in this snapshot.
     *
     * @return materialized server-deaf state
     */
    public boolean serverDeaf() {
        return serverDeaf;
    }

    /**
     * Returns whether the participant has a materialized Mumble connection.
     *
     * @return connection presence at this snapshot revision
     */
    public boolean mumbleConnected() {
        return mumbleConnected;
    }
}
