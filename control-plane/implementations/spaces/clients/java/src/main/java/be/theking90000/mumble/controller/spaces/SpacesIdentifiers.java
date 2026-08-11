package be.theking90000.mumble.controller.spaces;

import java.util.Objects;

final class SpacesIdentifiers {
    private SpacesIdentifiers() {
    }

    static String requireIdentifier(String value, String name) {
        Objects.requireNonNull(value, name);
        String normalized = value.trim();
        if (normalized.isEmpty()) {
            throw new IllegalArgumentException(name + " must not be blank");
        }
        if (normalized.length() > 255) {
            throw new IllegalArgumentException(name + " must not exceed 255 characters");
        }
        return normalized;
    }
}
