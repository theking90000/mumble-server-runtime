package be.theking90000.mumble.controller.spaces;


import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Objects;

/**
 * Complete, immutable projection of one materialized space.
 *
 * <p>Snapshots replace rather than patch the previous cached value. Compare both {@link
 * #incarnation()} and {@link #spaceRevision()} when ordering observations for a semantic key.</p>
 */
public final class SpaceSnapshot {
    private final SpaceKey spaceKey;
    private final SpaceIncarnation incarnation;
    private final long spaceRevision;
    private final List<SpaceParticipant> participants;
    private final long publishedGeneration;

    SpaceSnapshot(
            SpaceKey spaceKey,
            SpaceIncarnation incarnation,
            long spaceRevision,
            List<SpaceParticipant> participants,
            long publishedGeneration) {
        this.spaceKey = Objects.requireNonNull(spaceKey, "spaceKey");
        this.incarnation = Objects.requireNonNull(incarnation, "incarnation");
        this.spaceRevision = spaceRevision;
        this.participants = Collections.unmodifiableList(new ArrayList<SpaceParticipant>(participants));
        this.publishedGeneration = publishedGeneration;
    }

    /**
     * Returns the semantic space key.
     *
     * @return semantic key
     */
    public SpaceKey spaceKey() {
        return spaceKey;
    }

    /**
     * Returns the opaque identity of this materialized lifetime.
     *
     * @return incarnation identity
     */
    public SpaceIncarnation incarnation() {
        return incarnation;
    }

    /**
     * Returns the full snapshot revision within this incarnation.
     *
     * @return unsigned space revision represented as a Java {@code long}
     */
    public long spaceRevision() {
        return spaceRevision;
    }

    /**
     * Returns the participants materialized in this full snapshot.
     *
     * @return immutable participant list
     */
    public List<SpaceParticipant> participants() {
        return participants;
    }

    /**
     * Returns the Mumble publication generation associated with this snapshot.
     *
     * @return unsigned publication generation represented as a Java {@code long}
     */
    public long publishedGeneration() {
        return publishedGeneration;
    }
}
