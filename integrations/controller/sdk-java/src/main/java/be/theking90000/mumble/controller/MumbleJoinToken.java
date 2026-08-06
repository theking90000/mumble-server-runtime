package be.theking90000.mumble.controller;

import java.util.Objects;

/**
 * Bearer credential used by one participant to authenticate an unmodified Mumble client.
 *
 * <p>This value is deliberately distinct from the internal ownership capability. Possessing it
 * permits a Mumble connection for the participant, but never permits controller mutations. Treat
 * the returned value as a password: do not log it, persist it in plaintext, or expose it to another
 * participant.</p>
 */
public final class MumbleJoinToken {
    private final String value;

    MumbleJoinToken(String value) {
        this.value = Objects.requireNonNull(value, "value");
        if (value.isEmpty()) {
            throw new IllegalArgumentException("value must not be empty");
        }
    }

    /**
     * Returns the credential to place in the Mumble password field.
     *
     * @return opaque bearer credential
     */
    public String value() {
        return value;
    }

    /**
     * Returns a redacted representation that never contains the credential.
     *
     * @return a constant redacted label
     */
    @Override
    public String toString() {
        return "MumbleJoinToken[REDACTED]";
    }
}
