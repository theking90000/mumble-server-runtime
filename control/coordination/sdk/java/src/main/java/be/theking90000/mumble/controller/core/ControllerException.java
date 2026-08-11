package be.theking90000.mumble.controller.core;

/** Base unchecked exception for local validation, protocol, and asynchronous controller failures. */
public class ControllerException extends RuntimeException {
    private static final long serialVersionUID = 1L;

    /**
     * Creates a controller exception with a descriptive message.
     *
     * @param message description of the failure
     */
    public ControllerException(String message) {
        super(message);
    }

    /**
     * Creates a controller exception with a descriptive message and underlying cause.
     *
     * @param message description of the failure
     * @param cause underlying failure
     */
    public ControllerException(String message, Throwable cause) {
        super(message, cause);
    }
}
