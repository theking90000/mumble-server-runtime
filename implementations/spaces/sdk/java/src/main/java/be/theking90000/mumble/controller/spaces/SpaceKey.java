package be.theking90000.mumble.controller.spaces;


import java.util.Objects;

/**
 * Semantic key of a dynamic voice space.
 *
 * <p>The runtime materializes and closes spaces according to policy. A key names the semantic
 * destination; it is not ownership of a long-lived server object.</p>
 */
public final class SpaceKey implements Comparable<SpaceKey> {
    private final String value;

    private SpaceKey(String value) {
        this.value = SpacesIdentifiers.requireIdentifier(value, "spaceKey");
    }

    /**
     * Creates a semantic space key.
     *
     * @param value non-blank wire value
     * @return an immutable key
     * @throws NullPointerException if {@code value} is {@code null}
     * @throws IllegalArgumentException if {@code value} is blank
     */
    public static SpaceKey of(String value) {
        return new SpaceKey(value);
    }

    /**
     * Returns the exact key supplied to {@link #of(String)}.
     *
     * @return the wire value
     */
    public String value() {
        return value;
    }

    /** {@inheritDoc} */
    @Override
    public int compareTo(SpaceKey other) {
        return value.compareTo(other.value);
    }

    /** {@inheritDoc} */
    @Override
    public boolean equals(Object other) {
        return this == other || other instanceof SpaceKey
                && value.equals(((SpaceKey) other).value);
    }

    /** {@inheritDoc} */
    @Override
    public int hashCode() {
        return Objects.hash(value);
    }

    /** @return the wire value */
    @Override
    public String toString() {
        return value;
    }
}
