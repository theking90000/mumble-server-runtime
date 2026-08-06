package io.github.theking90000.mumbleserverruntime.controller;

import java.util.Objects;

/** Complete desired state of a participant. Updates replace this value atomically. */
public final class ParticipantSpec {
    private final SpaceKey spaceKey;
    private final String displayName;
    private final boolean serverMute;
    private final boolean serverDeaf;

    private ParticipantSpec(Builder builder) {
        spaceKey = Objects.requireNonNull(builder.spaceKey, "spaceKey");
        displayName = ControllerId.requireIdentifier(builder.displayName, "displayName");
        serverMute = builder.serverMute;
        serverDeaf = builder.serverDeaf;
    }

    public static Builder builder(SpaceKey spaceKey, String displayName) {
        return new Builder(spaceKey, displayName);
    }

    public SpaceKey spaceKey() {
        return spaceKey;
    }

    public String displayName() {
        return displayName;
    }

    public boolean serverMute() {
        return serverMute;
    }

    public boolean serverDeaf() {
        return serverDeaf;
    }

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

        public Builder serverMute(boolean value) {
            serverMute = value;
            return this;
        }

        public Builder serverDeaf(boolean value) {
            serverDeaf = value;
            return this;
        }

        public ParticipantSpec build() {
            return new ParticipantSpec(this);
        }
    }
}
