package be.theking90000.mumble.controller;

import com.google.protobuf.ByteString;
import java.util.Arrays;
import java.util.Objects;

/** Immutable encoded Protobuf payload interpreted only by the negotiated profile. */
public final class ProfilePayload {
    private static final ProfilePayload EMPTY = new ProfilePayload(new byte[0]);

    private final byte[] protobuf;

    private ProfilePayload(byte[] protobuf) {
        this.protobuf = protobuf;
    }

    /**
     * Returns the canonical empty payload.
     *
     * @return empty payload
     */
    public static ProfilePayload empty() {
        return EMPTY;
    }

    /**
     * Copies encoded Protobuf bytes into an immutable payload.
     *
     * @param protobuf encoded profile message
     * @return immutable payload
     * @throws NullPointerException if {@code protobuf} is {@code null}
     */
    public static ProfilePayload of(byte[] protobuf) {
        Objects.requireNonNull(protobuf, "protobuf");
        if (protobuf.length == 0) {
            return EMPTY;
        }
        return new ProfilePayload(Arrays.copyOf(protobuf, protobuf.length));
    }

    /**
     * Returns a defensive copy of the encoded Protobuf bytes.
     *
     * @return encoded profile message
     */
    public byte[] protobuf() {
        return Arrays.copyOf(protobuf, protobuf.length);
    }

    ByteString toByteString() {
        return ByteString.copyFrom(protobuf);
    }

    static ProfilePayload fromByteString(ByteString protobuf) {
        return protobuf.isEmpty() ? EMPTY : new ProfilePayload(protobuf.toByteArray());
    }
}
