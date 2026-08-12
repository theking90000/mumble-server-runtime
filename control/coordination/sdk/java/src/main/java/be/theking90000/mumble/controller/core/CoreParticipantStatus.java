package be.theking90000.mumble.controller.core;

import java.util.Objects;
import java.util.Optional;

/** Last complete generic status reported for an owned participant. */
public final class CoreParticipantStatus {
    private final boolean connected;
    private final long acceptedSpecRevision;
    private final long appliedSpecRevision;
    private final long publishedGeneration;
    private final String applicationError;
    private final ProfilePayload profileStatus;

    CoreParticipantStatus(
            boolean connected,
            long acceptedSpecRevision,
            long appliedSpecRevision,
            long publishedGeneration,
            String applicationError,
            ProfilePayload profileStatus) {
        this.connected = connected;
        this.acceptedSpecRevision = acceptedSpecRevision;
        this.appliedSpecRevision = appliedSpecRevision;
        this.publishedGeneration = publishedGeneration;
        this.applicationError = Objects.requireNonNull(applicationError, "applicationError");
        this.profileStatus = Objects.requireNonNull(profileStatus, "profileStatus");
    }

    /** @return whether the Host currently has an attached connection */
    public boolean connected() {
        return connected;
    }

    /** @return latest specification revision accepted by the profile */
    public long acceptedSpecRevision() {
        return acceptedSpecRevision;
    }

    /** @return latest accepted revision applied to local runtime state */
    public long appliedSpecRevision() {
        return appliedSpecRevision;
    }

    /** @return latest runtime publication generation */
    public long publishedGeneration() {
        return publishedGeneration;
    }

    /** @return current application error, if profile application failed */
    public Optional<String> applicationError() {
        return applicationError.isEmpty()
                ? Optional.<String>empty()
                : Optional.of(applicationError);
    }

    /** @return complete profile-owned status payload */
    public ProfilePayload profileStatus() {
        return profileStatus;
    }
}
