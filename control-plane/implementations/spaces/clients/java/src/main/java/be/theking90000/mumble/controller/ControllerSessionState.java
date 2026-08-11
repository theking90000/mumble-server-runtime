package be.theking90000.mumble.controller;

/** Observable lifecycle of a controller session. */
public enum ControllerSessionState {
    /** The session has been built but not started. */
    NEW,
    /** A transport connection is being established. */
    CONNECTING,
    /** The runtime is examining the complete desired-state snapshot. */
    RECONCILING,
    /** Reconciliation completed and commands can be sent. */
    ACTIVE,
    /** A retryable stream failure suspended remote operations. */
    RECONNECTING,
    /** A graceful stop has begun. */
    STOPPING,
    /** The session is permanently closed. */
    CLOSED,
    /** A permanent transport or protocol failure terminated the session. */
    FAILED
}
