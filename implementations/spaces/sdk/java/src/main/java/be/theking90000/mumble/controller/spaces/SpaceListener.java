package be.theking90000.mumble.controller.spaces;


/** Receives full space replacements and incarnation closures. */
public interface SpaceListener {
    /**
     * Called when a complete space snapshot replaces the cached value for its key.
     *
     * @param session session receiving the snapshot
     * @param snapshot complete immutable replacement
     */
    void onSpaceUpdated(ControllerSession session, SpaceSnapshot snapshot);

    /**
     * Called when one particular space incarnation closes.
     *
     * <p>The same semantic key may later reappear with another incarnation identifier.</p>
     *
     * @param session session receiving the closure
     * @param spaceKey semantic key of the closed space
     * @param incarnation exact materialization that closed
     * @param finalRevision final unsigned revision of that incarnation
     */
    default void onSpaceClosed(
            ControllerSession session,
            SpaceKey spaceKey,
            SpaceIncarnation incarnation,
            long finalRevision) {
    }
}
