package be.theking90000.mumble.controller;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import org.junit.jupiter.api.Test;

final class CoreSessionLifecycleTest {
    @Test
    void lifecycle_accepts_reconnect_and_resynchronization_paths() {
        CoreSessionLifecycle lifecycle = new CoreSessionLifecycle();

        assertEquals(ControllerSessionState.NEW, lifecycle.state());
        assertEquals(
                ControllerSessionState.NEW,
                lifecycle.transitionTo(ControllerSessionState.CONNECTING));
        lifecycle.transitionTo(ControllerSessionState.RECONCILING);
        lifecycle.transitionTo(ControllerSessionState.ACTIVE);
        lifecycle.transitionTo(ControllerSessionState.RECONCILING);
        lifecycle.transitionTo(ControllerSessionState.RECONNECTING);
        lifecycle.transitionTo(ControllerSessionState.RECONCILING);
        lifecycle.transitionTo(ControllerSessionState.ACTIVE);

        assertEquals(ControllerSessionState.ACTIVE, lifecycle.state());
    }

    @Test
    void lifecycle_rejects_transitions_out_of_closed() {
        CoreSessionLifecycle lifecycle = new CoreSessionLifecycle();
        lifecycle.transitionTo(ControllerSessionState.CLOSED);

        assertThrows(
                IllegalStateException.class,
                () -> lifecycle.transitionTo(ControllerSessionState.CONNECTING));
    }

    @Test
    void reconnect_backoff_is_bounded_and_resets_after_activation() {
        CoreSessionLifecycle lifecycle = new CoreSessionLifecycle();

        assertEquals(200L, lifecycle.nextReconnectDelayMillis(0.0d));
        assertEquals(500L, lifecycle.nextReconnectDelayMillis(0.5d));
        for (int attempt = 0; attempt < 30; attempt++) {
            lifecycle.nextReconnectDelayMillis(0.5d);
        }
        assertEquals(12_000L, lifecycle.nextReconnectDelayMillis(Math.nextDown(1.0d)));

        lifecycle.resetReconnectBackoff();
        assertEquals(250L, lifecycle.nextReconnectDelayMillis(0.5d));
    }

    @Test
    void reconnect_backoff_rejects_invalid_jitter_samples() {
        CoreSessionLifecycle lifecycle = new CoreSessionLifecycle();

        assertThrows(IllegalArgumentException.class, () -> lifecycle.nextReconnectDelayMillis(-0.1d));
        assertThrows(IllegalArgumentException.class, () -> lifecycle.nextReconnectDelayMillis(1.0d));
        assertThrows(IllegalArgumentException.class, () -> lifecycle.nextReconnectDelayMillis(Double.NaN));
    }
}
