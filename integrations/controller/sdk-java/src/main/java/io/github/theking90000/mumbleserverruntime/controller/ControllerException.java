package io.github.theking90000.mumbleserverruntime.controller;

/** Base exception for asynchronous controller failures. */
public class ControllerException extends RuntimeException {
    private static final long serialVersionUID = 1L;

    public ControllerException(String message) {
        super(message);
    }

    public ControllerException(String message, Throwable cause) {
        super(message, cause);
    }
}
