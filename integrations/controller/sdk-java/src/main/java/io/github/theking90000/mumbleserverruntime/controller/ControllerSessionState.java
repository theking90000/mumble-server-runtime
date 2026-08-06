package io.github.theking90000.mumbleserverruntime.controller;

/** Observable lifecycle of a controller session. */
public enum ControllerSessionState {
    NEW,
    CONNECTING,
    RECONCILING,
    ACTIVE,
    RECONNECTING,
    STOPPING,
    CLOSED,
    FAILED
}
