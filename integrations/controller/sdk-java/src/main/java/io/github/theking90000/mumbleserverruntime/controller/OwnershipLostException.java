package io.github.theking90000.mumbleserverruntime.controller;

/** The participant capability was revoked or replaced by another controller. */
public final class OwnershipLostException extends ControllerException {
    private static final long serialVersionUID = 1L;

    public OwnershipLostException(ParticipantId participantId, String reason) {
        super("Ownership lost for " + participantId + ": " + reason);
    }
}
