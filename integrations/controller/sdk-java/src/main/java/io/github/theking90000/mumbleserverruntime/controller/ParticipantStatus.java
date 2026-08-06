package io.github.theking90000.mumbleserverruntime.controller;

import java.util.Optional;

/** Last status reported by Rust for an owned participant. */
public final class ParticipantStatus {
    private final boolean mumbleConnected;
    private final SpaceKey appliedSpaceKey;
    private final boolean selfMute;
    private final boolean selfDeaf;
    private final long acceptedSpecRevision;
    private final long appliedSpecRevision;
    private final long publishedGeneration;
    private final String applicationError;

    ParticipantStatus(
            boolean mumbleConnected,
            SpaceKey appliedSpaceKey,
            boolean selfMute,
            boolean selfDeaf,
            long acceptedSpecRevision,
            long appliedSpecRevision,
            long publishedGeneration,
            String applicationError) {
        this.mumbleConnected = mumbleConnected;
        this.appliedSpaceKey = appliedSpaceKey;
        this.selfMute = selfMute;
        this.selfDeaf = selfDeaf;
        this.acceptedSpecRevision = acceptedSpecRevision;
        this.appliedSpecRevision = appliedSpecRevision;
        this.publishedGeneration = publishedGeneration;
        this.applicationError = applicationError;
    }

    public boolean mumbleConnected() {
        return mumbleConnected;
    }

    public Optional<SpaceKey> appliedSpaceKey() {
        return Optional.ofNullable(appliedSpaceKey);
    }

    public boolean selfMute() {
        return selfMute;
    }

    public boolean selfDeaf() {
        return selfDeaf;
    }

    public long acceptedSpecRevision() {
        return acceptedSpecRevision;
    }

    public long appliedSpecRevision() {
        return appliedSpecRevision;
    }

    public long publishedGeneration() {
        return publishedGeneration;
    }

    public Optional<String> applicationError() {
        return applicationError == null || applicationError.isEmpty()
                ? Optional.<String>empty()
                : Optional.of(applicationError);
    }
}
