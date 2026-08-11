package be.theking90000.mumble.controller;

import java.util.Objects;

/** Exact typed profile schema negotiated by a Controller session. */
public final class ProfileReference {
    private final String profileId;
    private final int schemaVersion;
    private final String descriptorDigest;

    private ProfileReference(String profileId, int schemaVersion, String descriptorDigest) {
        this.profileId = ControllerId.requireIdentifier(profileId, "profileId");
        if (schemaVersion <= 0) {
            throw new IllegalArgumentException("schemaVersion must be positive");
        }
        this.schemaVersion = schemaVersion;
        this.descriptorDigest = Objects.requireNonNull(descriptorDigest, "descriptorDigest");
        if (!descriptorDigest.matches("[0-9a-f]{64}")) {
            throw new IllegalArgumentException(
                    "descriptorDigest must contain 64 lowercase hexadecimal characters");
        }
    }

    /**
     * Creates an exact profile schema reference.
     *
     * @param profileId stable profile identifier
     * @param schemaVersion positive schema version
     * @param descriptorDigest lowercase SHA-256 descriptor digest
     * @return validated profile reference
     */
    public static ProfileReference of(
            String profileId, int schemaVersion, String descriptorDigest) {
        return new ProfileReference(profileId, schemaVersion, descriptorDigest);
    }

    /** @return stable profile identifier */
    public String profileId() {
        return profileId;
    }

    /** @return positive schema version */
    public int schemaVersion() {
        return schemaVersion;
    }

    /** @return lowercase SHA-256 descriptor digest */
    public String descriptorDigest() {
        return descriptorDigest;
    }
}
