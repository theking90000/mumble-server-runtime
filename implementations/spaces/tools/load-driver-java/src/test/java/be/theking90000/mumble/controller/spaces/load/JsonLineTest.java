package be.theking90000.mumble.controller.spaces.load;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;

import be.theking90000.mumble.controller.core.ControllerId;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

final class JsonLineTest {
    @Test
    void parsesTheVersionedFlatCommandContract() {
        JsonLine line = JsonLine.parse(
                "{\"schema_version\":1,\"kind\":\"set_spec\","
                        + "\"server_mute\":true,\"display_name\":\"Alice \\\"A\\\"\"}");

        assertEquals(1, line.requiredInt("schema_version"));
        assertEquals("set_spec", line.required("kind"));
        assertEquals("Alice \"A\"", line.required("display_name"));
        assertEquals(true, line.optionalBoolean("server_mute", false));
        assertEquals(false, line.optionalBoolean("server_deaf", false));
    }

    @Test
    void rejectsNestedAndDuplicateInput() {
        assertThrows(IllegalArgumentException.class,
                () -> JsonLine.parse("{\"spec\":{\"space\":\"a\"}}"));
        assertThrows(IllegalArgumentException.class,
                () -> JsonLine.parse("{\"kind\":\"a\",\"kind\":\"b\"}"));
    }

    @Test
    void writerEscapesStringsWithoutInventingCredentialFields() {
        Map<String, Object> fields = new LinkedHashMap<String, Object>();
        fields.put("kind", "participant_status");
        fields.put("message", "line one\nline two");

        String encoded = JsonLine.object(fields);

        assertEquals(
                "{\"kind\":\"participant_status\",\"message\":\"line one\\nline two\"}",
                encoded);
        assertFalse(encoded.contains("credential"));
    }

    @Test
    void parsesDistinctControllerIdsForOneDriverProcess() {
        List<ControllerId> controllers = DriverMain.parseControllerIds(
                "load-controller-0,load-controller-1");

        assertEquals(2, controllers.size());
        assertEquals("load-controller-0", controllers.get(0).value());
        assertEquals("load-controller-1", controllers.get(1).value());
        assertThrows(IllegalArgumentException.class,
                () -> DriverMain.parseControllerIds("load-controller-0,load-controller-0"));
        assertThrows(IllegalArgumentException.class,
                () -> DriverMain.parseControllerIds("load-controller-0,"));
    }
}
