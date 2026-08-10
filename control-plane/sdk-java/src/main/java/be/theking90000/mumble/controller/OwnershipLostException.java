package be.theking90000.mumble.controller;

/** The participant capability was revoked or replaced by another controller. */
public final class OwnershipLostException extends ControllerException {
    private static final long serialVersionUID = 1L;

    /**
     * Creates a terminal ownership-loss failure.
     *
     * @param participantId participant whose capability was revoked
     * @param reason runtime-provided revocation reason
     */
    public OwnershipLostException(ParticipantId participantId, String reason) {
        super("Ownership lost for " + participantId + ": " + reason);
    }
}
