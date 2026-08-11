package be.theking90000.mumble.controller;

/** Receives serialized generic participant lifecycle notifications. */
public interface CoreParticipantListener {
    /**
     * Called after the local ownership state changes.
     *
     * @param participant participant whose state changed
     * @param previous previous state
     * @param current current state
     */
    default void onStateChanged(
            CoreParticipantHandle participant,
            ParticipantHandleState previous,
            ParticipantHandleState current) {
    }

    /**
     * Called when a complete generic participant status arrives.
     *
     * @param participant participant whose status changed
     * @param status complete generic status
     */
    default void onStatusChanged(
            CoreParticipantHandle participant, CoreParticipantStatus status) {
    }

    /**
     * Called when ownership yields a different connection credential.
     *
     * @param participant participant whose credential changed
     * @param credential new connection credential
     */
    default void onConnectionCredentialChanged(
            CoreParticipantHandle participant, ConnectionCredential credential) {
    }

    /**
     * Called when the runtime fences this ownership.
     *
     * @param participant participant that lost ownership
     * @param reason protocol revocation reason
     */
    default void onOwnershipLost(CoreParticipantHandle participant, String reason) {
    }
}
