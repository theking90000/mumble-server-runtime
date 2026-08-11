package be.theking90000.mumble.controller.spaces;

import be.theking90000.mumble.controller.core.ControllerSessionState;

/** Receives serialized session lifecycle notifications. */
public interface ControllerSessionListener {
    /**
     * Called after the observable session state changes.
     *
     * <p>Callbacks are serialized on the session callback executor. Implementations should avoid
     * blocking that executor.</p>
     *
     * @param session session whose state changed
     * @param previous state before the transition
     * @param current state after the transition
     */
    void onStateChanged(ControllerSession session, ControllerSessionState previous, ControllerSessionState current);
}
