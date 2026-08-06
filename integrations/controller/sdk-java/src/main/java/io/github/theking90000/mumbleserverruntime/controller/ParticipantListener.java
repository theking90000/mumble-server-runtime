package io.github.theking90000.mumbleserverruntime.controller;

/** Receives serialized participant lifecycle and status notifications. */
public interface ParticipantListener {
    default void onStateChanged(
            ParticipantHandle participant,
            ParticipantHandleState previous,
            ParticipantHandleState current) {
    }

    default void onStatusChanged(ParticipantHandle participant, ParticipantStatus status) {
    }

    default void onOwnershipLost(ParticipantHandle participant, String reason) {
    }
}
