package io.github.theking90000.mumbleserverruntime.controller;

import java.util.Arrays;

/** Opaque identity of one materialization of a semantic space key. */
public final class SpaceIncarnation {
    private final byte[] value;

    SpaceIncarnation(byte[] value) {
        this.value = value.clone();
    }

    public byte[] value() {
        return value.clone();
    }

    @Override
    public boolean equals(Object other) {
        return this == other || other instanceof SpaceIncarnation
                && Arrays.equals(value, ((SpaceIncarnation) other).value);
    }

    @Override
    public int hashCode() {
        return Arrays.hashCode(value);
    }
}
