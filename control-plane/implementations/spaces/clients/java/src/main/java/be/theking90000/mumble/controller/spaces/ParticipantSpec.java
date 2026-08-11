package be.theking90000.mumble.controller.spaces;


import java.util.Objects;

/**
 * Complete desired state of a participant.
 *
 * <p>Updates replace this value atomically. In particular, changing {@link #spaceKey()} expresses
 * placement without a separate move operation.</p>
 */
public final class ParticipantSpec {
    private final SpaceKey spaceKey;
    private final String displayName;
    private final boolean serverMute;
    private final boolean serverDeaf;

    private ParticipantSpec(Builder builder) {
        spaceKey = Objects.requireNonNull(builder.spaceKey, "spaceKey");
        displayName = SpacesIdentifiers.requireIdentifier(builder.displayName, "displayName");
        serverMute = builder.serverMute;
        serverDeaf = builder.serverDeaf;
    }

    /**
     * Creates a builder with the required placement and display name.
     *
     * @param spaceKey desired semantic voice space
     * @param displayName non-blank name to publish for the participant
     * @return a new builder with both mute flags disabled
     * @throws NullPointerException if {@code spaceKey} is {@code null}
     */
    public static Builder builder(SpaceKey spaceKey, String displayName) {
        return new Builder(spaceKey, displayName);
    }

    /**
     * Returns the desired semantic voice space.
     *
     * @return desired space key
     */
    public SpaceKey spaceKey() {
        return spaceKey;
    }

    /**
     * Returns the desired published display name.
     *
     * @return non-blank display name
     */
    public String displayName() {
        return displayName;
    }

    /**
     * Returns whether the server should prevent this participant from speaking.
     *
     * @return desired server-mute state
     */
    public boolean serverMute() {
        return serverMute;
    }

    /**
     * Returns whether the server should prevent this participant from hearing voice.
     *
     * @return desired server-deaf state
     */
    public boolean serverDeaf() {
        return serverDeaf;
    }

    /** {@inheritDoc} */
    @Override
    public boolean equals(Object other) {
        if (this == other) {
            return true;
        }
        if (!(other instanceof ParticipantSpec)) {
            return false;
        }
        ParticipantSpec that = (ParticipantSpec) other;
        return serverMute == that.serverMute
                && serverDeaf == that.serverDeaf
                && spaceKey.equals(that.spaceKey)
                && displayName.equals(that.displayName);
    }

    /** {@inheritDoc} */
    @Override
    public int hashCode() {
        return Objects.hash(spaceKey, displayName, serverMute, serverDeaf);
    }

    /** Builder for an immutable participant spec. */
    public static final class Builder {
        private final SpaceKey spaceKey;
        private final String displayName;
        private boolean serverMute;
        private boolean serverDeaf;

        private Builder(SpaceKey spaceKey, String displayName) {
            this.spaceKey = Objects.requireNonNull(spaceKey, "spaceKey");
            this.displayName = displayName;
        }

        /**
         * Sets the desired server-mute state.
         *
         * @param value whether the server should prevent speaking
         * @return this builder
         */
        public Builder serverMute(boolean value) {
            serverMute = value;
            return this;
        }

        /**
         * Sets the desired server-deaf state.
         *
         * @param value whether the server should prevent hearing voice
         * @return this builder
         */
        public Builder serverDeaf(boolean value) {
            serverDeaf = value;
            return this;
        }

        /**
         * Builds an immutable complete specification.
         *
         * @return the participant specification
         * @throws NullPointerException if a required value is {@code null}
         * @throws IllegalArgumentException if the display name is blank
         */
        public ParticipantSpec build() {
            return new ParticipantSpec(this);
        }
    }
}
