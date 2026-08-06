package io.github.theking90000.mumbleserverruntime.controller;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Objects;

/** Complete, immutable projection of one materialized space. */
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

    public SpaceKey spaceKey() {
        return spaceKey;
    }

    public SpaceIncarnation incarnation() {
        return incarnation;
    }

    public long spaceRevision() {
        return spaceRevision;
    }

    public List<SpaceParticipant> participants() {
        return participants;
    }

    public long publishedGeneration() {
        return publishedGeneration;
    }
}
