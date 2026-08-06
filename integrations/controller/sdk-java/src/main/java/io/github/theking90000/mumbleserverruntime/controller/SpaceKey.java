package io.github.theking90000.mumbleserverruntime.controller;

import java.util.Objects;

/** Semantic key of a dynamic voice space. */
public final class SpaceKey implements Comparable<SpaceKey> {
    private final String value;

    private SpaceKey(String value) {
        this.value = ControllerId.requireIdentifier(value, "spaceKey");
    }

    public static SpaceKey of(String value) {
        return new SpaceKey(value);
    }

    public String value() {
        return value;
    }

    @Override
    public int compareTo(SpaceKey other) {
        return value.compareTo(other.value);
    }

    @Override
    public boolean equals(Object other) {
        return this == other || other instanceof SpaceKey
                && value.equals(((SpaceKey) other).value);
    }

    @Override
    public int hashCode() {
        return Objects.hash(value);
    }

    @Override
    public String toString() {
        return value;
    }
}
