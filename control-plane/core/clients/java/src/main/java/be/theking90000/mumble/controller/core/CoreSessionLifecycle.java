package be.theking90000.mumble.controller.core;

import java.util.EnumSet;
import java.util.Objects;

/** Owns the profile-neutral session lifecycle and reconnect backoff policy. */
final class CoreSessionLifecycle {
    private static final long INITIAL_RECONNECT_MILLIS = 250L;
    private static final long MAX_RECONNECT_MILLIS = 10_000L;
    private static final double RECONNECT_JITTER = 0.20d;

    private ControllerSessionState state = ControllerSessionState.NEW;
    private int reconnectAttempt;

    ControllerSessionState state() {
        return state;
    }

    ControllerSessionState transitionTo(ControllerSessionState next) {
        Objects.requireNonNull(next, "next");
        ControllerSessionState previous = state;
        if (previous == next) {
            return previous;
        }
        if (!allowedSuccessors(previous).contains(next)) {
            throw new IllegalStateException(
                    "invalid controller session transition: " + previous + " -> " + next);
        }
        state = next;
        return previous;
    }

    long nextReconnectDelayMillis(double jitterSample) {
        if (jitterSample < 0.0d || jitterSample >= 1.0d || Double.isNaN(jitterSample)) {
            throw new IllegalArgumentException("jitter sample must be in [0, 1)");
        }
        long exponent = 1L << Math.min(reconnectAttempt, 20);
        long base = Math.min(MAX_RECONNECT_MILLIS, INITIAL_RECONNECT_MILLIS * exponent);
        double factor = 1.0d - RECONNECT_JITTER + jitterSample * RECONNECT_JITTER * 2.0d;
        reconnectAttempt++;
        return Math.max(1L, Math.round(base * factor));
    }

    void resetReconnectBackoff() {
        reconnectAttempt = 0;
    }

    private static EnumSet<ControllerSessionState> allowedSuccessors(
            ControllerSessionState current) {
        switch (current) {
            case NEW:
                return EnumSet.of(
                        ControllerSessionState.CONNECTING,
                        ControllerSessionState.CLOSED,
                        ControllerSessionState.FAILED);
            case CONNECTING:
                return EnumSet.of(
                        ControllerSessionState.RECONCILING,
                        ControllerSessionState.RECONNECTING,
                        ControllerSessionState.STOPPING,
                        ControllerSessionState.CLOSED,
                        ControllerSessionState.FAILED);
            case RECONCILING:
            case ACTIVE:
                return EnumSet.of(
                        ControllerSessionState.RECONCILING,
                        ControllerSessionState.ACTIVE,
                        ControllerSessionState.RECONNECTING,
                        ControllerSessionState.STOPPING,
                        ControllerSessionState.CLOSED,
                        ControllerSessionState.FAILED);
            case RECONNECTING:
                return EnumSet.of(
                        ControllerSessionState.RECONCILING,
                        ControllerSessionState.RECONNECTING,
                        ControllerSessionState.STOPPING,
                        ControllerSessionState.CLOSED,
                        ControllerSessionState.FAILED);
            case STOPPING:
            case FAILED:
                return EnumSet.of(ControllerSessionState.CLOSED);
            case CLOSED:
            default:
                return EnumSet.noneOf(ControllerSessionState.class);
        }
    }
}
