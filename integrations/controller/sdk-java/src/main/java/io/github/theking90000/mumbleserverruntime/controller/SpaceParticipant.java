package io.github.theking90000.mumbleserverruntime.controller;

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

    public ParticipantId participantId() {
        return participantId;
    }

    public String displayName() {
        return displayName;
    }

    public boolean serverMute() {
        return serverMute;
    }

    public boolean serverDeaf() {
        return serverDeaf;
    }

    public boolean mumbleConnected() {
        return mumbleConnected;
    }
}
