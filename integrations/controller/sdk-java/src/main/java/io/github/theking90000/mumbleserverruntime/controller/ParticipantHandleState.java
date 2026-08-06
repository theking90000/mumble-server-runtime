package io.github.theking90000.mumbleserverruntime.controller;

/** Local lifecycle of one participant registration. */
public enum ParticipantHandleState {
    ACQUIRING,
    OWNED,
    SUSPENDED,
    REVOKED,
    CLOSED
}
