package be.theking90000.mumble.controller;

import java.util.Objects;

/** Logical identity of a participant, independent from transient connection identifiers. */
public final class ParticipantId {
    private final String value;

    private ParticipantId(String value) {
        this.value = ControllerId.requireIdentifier(value, "participantId");
    }

    /**
     * Creates an identifier from its wire representation.
     *
     * @param value non-blank participant identity, commonly a Minecraft player UUID
     * @return an immutable identifier
     * @throws NullPointerException if {@code value} is {@code null}
     * @throws IllegalArgumentException if {@code value} is blank
     */
    public static ParticipantId of(String value) {
        return new ParticipantId(value);
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
        return this == other || other instanceof ParticipantId
                && value.equals(((ParticipantId) other).value);
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
