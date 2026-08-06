package io.github.theking90000.mumbleserverruntime.controller;

import java.util.Objects;

/** Stable, declarative identity of a controller deployment. */
public final class ControllerId {
    private final String value;

    private ControllerId(String value) {
        this.value = requireIdentifier(value, "controllerId");
    }

    public static ControllerId of(String value) {
        return new ControllerId(value);
    }

    public String value() {
        return value;
    }

    @Override
    public boolean equals(Object other) {
        return this == other || other instanceof ControllerId
                && value.equals(((ControllerId) other).value);
    }

    @Override
    public int hashCode() {
        return Objects.hash(value);
    }

    @Override
    public String toString() {
        return value;
    }

    static String requireIdentifier(String value, String name) {
        Objects.requireNonNull(value, name);
        if (value.trim().isEmpty()) {
            throw new IllegalArgumentException(name + " must not be blank");
        }
        return value;
    }
}
