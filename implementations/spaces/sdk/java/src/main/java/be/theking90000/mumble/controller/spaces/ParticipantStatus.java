package be.theking90000.mumble.controller.spaces;


import java.util.Optional;

/**
 * Last complete status reported by the runtime for an owned participant.
 *
 * <p>This is observed state, not the desired {@link ParticipantSpec}. Revisions and publication
 * generation are unsigned 64-bit values represented as Java {@code long}s.</p>
 */
public final class ParticipantStatus {
    private final boolean connected;
    private final SpaceKey appliedSpaceKey;
    private final boolean selfMute;
    private final boolean selfDeaf;
    private final long acceptedSpecRevision;
    private final long appliedSpecRevision;
    private final long publishedGeneration;
    private final String applicationError;

    ParticipantStatus(
            boolean connected,
            SpaceKey appliedSpaceKey,
            boolean selfMute,
            boolean selfDeaf,
            long acceptedSpecRevision,
            long appliedSpecRevision,
            long publishedGeneration,
            String applicationError) {
        this.connected = connected;
        this.appliedSpaceKey = appliedSpaceKey;
        this.selfMute = selfMute;
        this.selfDeaf = selfDeaf;
        this.acceptedSpecRevision = acceptedSpecRevision;
        this.appliedSpecRevision = appliedSpecRevision;
        this.publishedGeneration = publishedGeneration;
        this.applicationError = applicationError;
    }

    /**
     * Returns whether the participant currently has a materialized Mumble connection.
     *
     * @return current connection presence
     */
    public boolean connected() {
        return connected;
    }

    /**
     * Returns the space placement most recently applied by the runtime.
     *
     * @return applied space, or empty if placement has not been applied
     */
    public Optional<SpaceKey> appliedSpaceKey() {
        return Optional.ofNullable(appliedSpaceKey);
    }

    /**
     * Returns the self-mute state reported by the Mumble participant.
     *
     * @return reported self-mute state
     */
    public boolean selfMute() {
        return selfMute;
    }

    /**
     * Returns the self-deaf state reported by the Mumble participant.
     *
     * @return reported self-deaf state
     */
    public boolean selfDeaf() {
        return selfDeaf;
    }

    /**
     * Returns the latest participant specification revision accepted by the runtime.
     *
     * @return accepted specification revision
     */
    public long acceptedSpecRevision() {
        return acceptedSpecRevision;
    }

    /**
     * Returns the latest accepted participant specification revision applied to runtime state.
     *
     * @return applied specification revision
     */
    public long appliedSpecRevision() {
        return appliedSpecRevision;
    }

    /**
     * Returns the latest Mumble publication generation reported with this status.
     *
     * @return publication generation
     */
    public long publishedGeneration() {
        return publishedGeneration;
    }

    /**
     * Returns the current application error, if the accepted desired state could not be applied.
     *
     * @return application error description, or empty when no error is reported
     */
    public Optional<String> applicationError() {
        return applicationError == null || applicationError.isEmpty()
                ? Optional.<String>empty()
                : Optional.of(applicationError);
    }
}
