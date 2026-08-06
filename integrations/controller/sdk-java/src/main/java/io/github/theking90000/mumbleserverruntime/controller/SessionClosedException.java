package io.github.theking90000.mumbleserverruntime.controller;

/** An operation cannot continue because the session is stopping or closed. */
public final class SessionClosedException extends ControllerException {
    private static final long serialVersionUID = 1L;

    public SessionClosedException(String message) {
        super(message);
    }
}
