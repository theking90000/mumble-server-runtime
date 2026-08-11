package be.theking90000.mumble.controller;

/** Local lifecycle of one participant registration. */
public enum ParticipantHandleState {
    /** Registration is desired but ownership has not yet been granted. */
    ACQUIRING,
    /** The session owns the participant and may replace its specification. */
    OWNED,
    /** Ownership is retained locally while the session reconnects and reconciles. */
    SUSPENDED,
    /** Ownership was transferred or invalidated; this handle cannot be reused. */
    REVOKED,
    /** Local unregistration began; this handle cannot be reused. */
    CLOSED
}
