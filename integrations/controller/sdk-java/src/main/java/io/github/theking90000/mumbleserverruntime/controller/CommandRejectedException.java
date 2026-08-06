package io.github.theking90000.mumbleserverruntime.controller;

/** Rust rejected one correlated command without closing the stream. */
public final class CommandRejectedException extends ControllerException {
    private static final long serialVersionUID = 1L;

    private final String code;

    CommandRejectedException(String code, String message) {
        super(code + ": " + message);
        this.code = code;
    }

    public String code() {
        return code;
    }
}
