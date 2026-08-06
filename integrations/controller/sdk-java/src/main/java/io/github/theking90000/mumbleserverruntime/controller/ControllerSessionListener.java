package io.github.theking90000.mumbleserverruntime.controller;

/** Receives serialized session lifecycle notifications. */
public interface ControllerSessionListener {
    void onStateChanged(ControllerSession session, ControllerSessionState previous, ControllerSessionState current);
}
