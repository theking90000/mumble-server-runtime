package be.theking90000.mumble.controller.core;

/** Receives serialized lifecycle notifications from a profile-neutral Core session. */
public interface CoreSessionListener {
    /**
     * Called after the observable session state changes.
     *
     * @param session session whose state changed
     * @param previous state before the transition
     * @param current state after the transition
     */
    void onStateChanged(
            CoreSession session,
            ControllerSessionState previous,
            ControllerSessionState current);
}
