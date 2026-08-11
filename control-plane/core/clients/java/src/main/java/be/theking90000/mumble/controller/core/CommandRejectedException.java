package be.theking90000.mumble.controller.core;

/** The runtime rejected one correlated command without closing the session. */
public final class CommandRejectedException extends ControllerException {
    private static final long serialVersionUID = 1L;

    /** Stable protocol rejection code. */
    private final String code;

    /**
     * Creates a rejection reported by either Core or the negotiated typed profile.
     *
     * @param code stable protocol error code
     * @param message human-readable diagnostic
     */
    public CommandRejectedException(String code, String message) {
        super(code + ": " + message);
        this.code = code;
    }

    /**
     * Returns the stable protocol error code supplied by the runtime.
     *
     * @return the rejection code
     */
    public String code() {
        return code;
    }
}
