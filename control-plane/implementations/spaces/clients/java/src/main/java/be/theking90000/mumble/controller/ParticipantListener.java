package be.theking90000.mumble.controller;

/** Receives serialized participant lifecycle and status notifications. */
public interface ParticipantListener {
    /**
     * Called after the local handle state changes.
     *
     * @param participant handle whose state changed
     * @param previous state before the transition
     * @param current state after the transition
     */
    default void onStateChanged(
            ParticipantHandle participant,
            ParticipantHandleState previous,
            ParticipantHandleState current) {
    }

    /**
     * Called when the runtime reports a replacement participant status.
     *
     * @param participant handle associated with the status
     * @param status latest complete participant status
     */
    default void onStatusChanged(ParticipantHandle participant, ParticipantStatus status) {
    }

    /**
     * Called when the runtime grants or rotates the participant's Mumble join credential.
     *
     * <p>The token is a bearer password and must not be logged. A rotation invalidates the value
     * delivered by an earlier invocation.</p>
     *
     * @param participant handle associated with the credential
     * @param token current Mumble join credential
     */
    default void onConnectionCredentialChanged(ParticipantHandle participant, ConnectionCredential token) {
    }

    /**
     * Called once when this handle permanently loses ownership.
     *
     * <p>A fresh call to {@link ControllerSession#registerParticipant(ParticipantId,
     * ParticipantSpec)} is required to express a new acquisition intent.</p>
     *
     * @param participant revoked handle
     * @param reason runtime-provided revocation reason
     */
    default void onOwnershipLost(ParticipantHandle participant, String reason) {
    }
}
