package be.theking90000.mumble.controller.core;

/**
 * Distinct runtime watermarks returned when a participant specification is accepted.
 *
 * <p>Protocol revisions are unsigned 64-bit values represented as Java {@code long}s. Use {@link
 * Long#compareUnsigned(long, long)} if application code must order them.</p>
 */
public final class AcceptedRevision {
    private final long clientSpecRevision;
    private final long acceptedSpecRevision;
    private final long appliedSpecRevision;
    private final long publishedGeneration;

    AcceptedRevision(
            long clientSpecRevision,
            long acceptedSpecRevision,
            long appliedSpecRevision,
            long publishedGeneration) {
        this.clientSpecRevision = clientSpecRevision;
        this.acceptedSpecRevision = acceptedSpecRevision;
        this.appliedSpecRevision = appliedSpecRevision;
        this.publishedGeneration = publishedGeneration;
    }

    /**
     * Returns the client-side specification revision covered by this acknowledgement.
     *
     * @return the acknowledged client revision
     */
    public long clientSpecRevision() {
        return clientSpecRevision;
    }

    /**
     * Returns the latest specification revision accepted by the runtime.
     *
     * @return the accepted runtime revision
     */
    public long acceptedSpecRevision() {
        return acceptedSpecRevision;
    }

    /**
     * Returns the latest accepted revision applied to runtime state.
     *
     * @return the applied runtime revision, which may lag the accepted revision
     */
    public long appliedSpecRevision() {
        return appliedSpecRevision;
    }

    /**
     * Returns the runtime publication generation visible when this acknowledgement was produced.
     *
     * @return the publication generation, which does not by itself prove that this spec is published
     */
    public long publishedGeneration() {
        return publishedGeneration;
    }
}
