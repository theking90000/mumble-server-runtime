package be.theking90000.mumble.controller.core;

import java.util.Objects;

/**
 * Stable, declarative identity of a controller deployment.
 *
 * <p>Several live controller instances may use the same identity. The protocol assigns each
 * controller session a separate instance identifier internally. This value is not an
 * authentication credential.</p>
 */
public final class ControllerId {
    private final String value;

    private ControllerId(String value) {
        this.value = requireIdentifier(value, "controllerId");
    }

    /**
     * Creates an identifier from its wire representation.
     *
     * @param value non-blank controller identity
     * @return an immutable identifier
     * @throws NullPointerException if {@code value} is {@code null}
     * @throws IllegalArgumentException if {@code value} is blank
     */
    public static ControllerId of(String value) {
        return new ControllerId(value);
    }

    /**
     * Returns the exact identifier supplied to {@link #of(String)}.
     *
     * @return the wire value
     */
    public String value() {
        return value;
    }

    /** {@inheritDoc} */
    @Override
    public boolean equals(Object other) {
        return this == other || other instanceof ControllerId
                && value.equals(((ControllerId) other).value);
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

    static String requireIdentifier(String value, String name) {
        Objects.requireNonNull(value, name);
        if (value.trim().isEmpty()) {
            throw new IllegalArgumentException(name + " must not be blank");
        }
        return value;
    }
}
