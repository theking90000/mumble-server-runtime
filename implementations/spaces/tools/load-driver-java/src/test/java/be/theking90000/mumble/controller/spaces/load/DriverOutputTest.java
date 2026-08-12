package be.theking90000.mumble.controller.spaces.load;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.PrintWriter;
import java.io.StringWriter;
import java.util.LinkedHashMap;
import java.util.Map;
import org.junit.jupiter.api.Test;

final class DriverOutputTest {
    @Test
    void milestoneEventsUseLatestWinsWithoutLosingCriticalOutput() throws Exception {
        StringWriter sink = new StringWriter();
        DriverOutput output = new DriverOutput(new PrintWriter(sink, false), 8, 4);
        output.emit("critical");
        output.emitLatest("participant:p1", event("status-one"));
        output.emitLatest("participant:p1", event("status-two"));

        output.start();
        output.close(5_000);

        String persisted = sink.toString();
        assertTrue(persisted.contains("critical"));
        assertTrue(persisted.contains("status-two"));
        assertFalse(persisted.contains("status-one"));
        assertEquals(1, output.snapshot().coalesced);
        assertFalse(output.failed());
    }

    @Test
    void rejectsUnknownEventModes() {
        assertEquals(DriverEventMode.FULL, DriverEventMode.parse("full"));
        assertEquals(DriverEventMode.MILESTONES, DriverEventMode.parse("milestones"));
        assertThrows(IllegalArgumentException.class, () -> DriverEventMode.parse("verbose"));
    }

    private static Map<String, Object> event(String value) {
        Map<String, Object> event = new LinkedHashMap<String, Object>();
        event.put("value", value);
        return event;
    }
}
