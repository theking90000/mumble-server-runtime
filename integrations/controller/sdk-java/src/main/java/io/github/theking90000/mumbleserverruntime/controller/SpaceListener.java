package io.github.theking90000.mumbleserverruntime.controller;

/** Receives full space replacements and incarnation closures. */
public interface SpaceListener {
    void onSpaceUpdated(ControllerSession session, SpaceSnapshot snapshot);

    default void onSpaceClosed(
            ControllerSession session,
            SpaceKey spaceKey,
            SpaceIncarnation incarnation,
            long finalRevision) {
    }
}
