package be.theking90000.mumble.controller;

/** An operation cannot continue because the session is stopping or closed. */
public final class SessionClosedException extends ControllerException {
    private static final long serialVersionUID = 1L;

    /**
     * Creates a failure for an operation attempted on closed desired state.
     *
     * @param message description of the closed session or participant handle
     */
    public SessionClosedException(String message) {
        super(message);
    }
}
