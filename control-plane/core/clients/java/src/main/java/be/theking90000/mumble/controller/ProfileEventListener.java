package be.theking90000.mumble.controller;

/** Receives uncorrelated events emitted by the negotiated profile. */
public interface ProfileEventListener {
    /**
     * Called with one complete encoded profile event.
     *
     * @param session source session
     * @param event encoded event payload
     */
    void onProfileEvent(CoreSession session, ProfilePayload event);
}
