package be.theking90000.mumble.controller.spaces.load;

import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Objects;

final class JsonLine {
    private final Map<String, String> fields;

    private JsonLine(Map<String, String> fields) {
        this.fields = Collections.unmodifiableMap(fields);
    }

    static JsonLine parse(String input) {
        Objects.requireNonNull(input, "input");
        Parser parser = new Parser(input);
        return new JsonLine(parser.parseObject());
    }

    String required(String name) {
        String value = fields.get(name);
        if (value == null) {
            throw new IllegalArgumentException("missing field: " + name);
        }
        return value;
    }

    String optional(String name, String fallback) {
        String value = fields.get(name);
        return value == null ? fallback : value;
    }

    boolean optionalBoolean(String name, boolean fallback) {
        String value = fields.get(name);
        if (value == null) {
            return fallback;
        }
        if ("true".equals(value)) {
            return true;
        }
        if ("false".equals(value)) {
            return false;
        }
        throw new IllegalArgumentException(name + " must be a boolean");
    }

    int requiredInt(String name) {
        String value = required(name);
        try {
            return Integer.parseInt(value);
        } catch (NumberFormatException failure) {
            throw new IllegalArgumentException(name + " must be an integer", failure);
        }
    }

    static String object(Map<String, ?> fields) {
        StringBuilder output = new StringBuilder("{");
        boolean first = true;
        for (Map.Entry<String, ?> entry : fields.entrySet()) {
            if (!first) {
                output.append(',');
            }
            first = false;
            appendString(output, entry.getKey());
            output.append(':');
            Object value = entry.getValue();
            if (value instanceof Boolean || value instanceof Number) {
                output.append(value);
            } else {
                appendString(output, String.valueOf(value));
            }
        }
        return output.append('}').toString();
    }

    private static void appendString(StringBuilder output, String value) {
        output.append('"');
        for (int index = 0; index < value.length(); index++) {
            char character = value.charAt(index);
            switch (character) {
                case '"': output.append("\\\""); break;
                case '\\': output.append("\\\\"); break;
                case '\n': output.append("\\n"); break;
                case '\r': output.append("\\r"); break;
                case '\t': output.append("\\t"); break;
                default:
                    if (character < 0x20) {
                        throw new IllegalArgumentException("control character in JSON string");
                    }
                    output.append(character);
                    break;
            }
        }
        output.append('"');
    }

    private static final class Parser {
        private final String input;
        private int offset;

        private Parser(String input) {
            this.input = input;
        }

        private Map<String, String> parseObject() {
            LinkedHashMap<String, String> result = new LinkedHashMap<String, String>();
            whitespace();
            require('{');
            whitespace();
            if (take('}')) {
                return result;
            }
            while (true) {
                whitespace();
                String key = string();
                whitespace();
                require(':');
                whitespace();
                String value = value();
                if (result.put(key, value) != null) {
                    throw new IllegalArgumentException("duplicate field: " + key);
                }
                whitespace();
                if (take('}')) {
                    whitespace();
                    if (offset != input.length()) {
                        throw new IllegalArgumentException("trailing data");
                    }
                    return result;
                }
                require(',');
            }
        }

        private String value() {
            if (peek() == '"') {
                return string();
            }
            int start = offset;
            while (offset < input.length()) {
                char character = input.charAt(offset);
                if (character == ',' || character == '}' || Character.isWhitespace(character)) {
                    break;
                }
                offset++;
            }
            if (start == offset) {
                throw new IllegalArgumentException("missing JSON value");
            }
            String value = input.substring(start, offset);
            if (!"true".equals(value) && !"false".equals(value)
                    && !value.matches("-?[0-9]+")) {
                throw new IllegalArgumentException("unsupported JSON value");
            }
            return value;
        }

        private String string() {
            require('"');
            StringBuilder result = new StringBuilder();
            while (offset < input.length()) {
                char character = input.charAt(offset++);
                if (character == '"') {
                    return result.toString();
                }
                if (character == '\\') {
                    if (offset >= input.length()) {
                        throw new IllegalArgumentException("incomplete JSON escape");
                    }
                    char escaped = input.charAt(offset++);
                    switch (escaped) {
                        case '"': result.append('"'); break;
                        case '\\': result.append('\\'); break;
                        case 'n': result.append('\n'); break;
                        case 'r': result.append('\r'); break;
                        case 't': result.append('\t'); break;
                        default: throw new IllegalArgumentException("unsupported JSON escape");
                    }
                } else {
                    result.append(character);
                }
            }
            throw new IllegalArgumentException("unterminated JSON string");
        }

        private char peek() {
            if (offset >= input.length()) {
                throw new IllegalArgumentException("unexpected end of JSON");
            }
            return input.charAt(offset);
        }

        private boolean take(char expected) {
            if (offset < input.length() && input.charAt(offset) == expected) {
                offset++;
                return true;
            }
            return false;
        }

        private void require(char expected) {
            if (!take(expected)) {
                throw new IllegalArgumentException("expected '" + expected + "' at " + offset);
            }
        }

        private void whitespace() {
            while (offset < input.length() && Character.isWhitespace(input.charAt(offset))) {
                offset++;
            }
        }
    }
}
