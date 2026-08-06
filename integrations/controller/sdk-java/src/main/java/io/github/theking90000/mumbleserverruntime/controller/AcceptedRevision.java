package io.github.theking90000.mumbleserverruntime.controller;

/** Distinct server watermarks for receipt, application, and Mumble publication. */
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

    public long clientSpecRevision() {
        return clientSpecRevision;
    }

    public long acceptedSpecRevision() {
        return acceptedSpecRevision;
    }

    public long appliedSpecRevision() {
        return appliedSpecRevision;
    }

    public long publishedGeneration() {
        return publishedGeneration;
    }
}
