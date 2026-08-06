package io.github.theking90000.mumbleserverruntime.controller;

import java.util.Objects;

/** Logical identity of a participant, independent from Mumble connection IDs. */
public final class ParticipantId {
    private final String value;

    private ParticipantId(String value) {
        this.value = ControllerId.requireIdentifier(value, "participantId");
    }

    public static ParticipantId of(String value) {
        return new ParticipantId(value);
    }

    public String value() {
        return value;
    }

    @Override
    public boolean equals(Object other) {
        return this == other || other instanceof ParticipantId
                && value.equals(((ParticipantId) other).value);
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
