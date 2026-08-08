package be.theking90000.mumble.controller;

import java.util.Arrays;

/**
 * Opaque identity of one materialization of a semantic space key.
 *
 * <p>A space may close and later be recreated under the same {@link SpaceKey}. The incarnation
 * prevents snapshots and revisions from those two lifetimes from being confused.</p>
 */
public final class SpaceIncarnation {
    private final byte[] value;

    SpaceIncarnation(byte[] value) {
        this.value = value.clone();
    }

    /**
     * Returns a defensive copy of the opaque wire value.
     *
     * @return incarnation bytes
     */
    public byte[] value() {
        return value.clone();
    }

    /** {@inheritDoc} */
    @Override
    public boolean equals(Object other) {
        return this == other || other instanceof SpaceIncarnation
                && Arrays.equals(value, ((SpaceIncarnation) other).value);
    }

    /** {@inheritDoc} */
    @Override
    public int hashCode() {
        return Arrays.hashCode(value);
    }
}
