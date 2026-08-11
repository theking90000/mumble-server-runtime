package be.theking90000.mumble.controller;

import com.google.protobuf.ByteString;
import java.util.HashMap;
import java.util.Map;
import java.util.Objects;
import java.util.function.Supplier;

/** Correlates reliable Core commands with their rejection handlers. */
final class ReliableRequestTracker {
    private final Supplier<ByteString> requestIds;
    private final Map<ByteString, RejectionHandler> handlers =
            new HashMap<ByteString, RejectionHandler>();

    ReliableRequestTracker(Supplier<ByteString> requestIds) {
        this.requestIds = Objects.requireNonNull(requestIds, "requestIds");
    }

    ByteString track(RejectionHandler handler) {
        ByteString requestId = Objects.requireNonNull(requestIds.get(), "requestId");
        if (requestId.isEmpty()) {
            throw new IllegalStateException("request id must not be empty");
        }
        if (handlers.put(requestId, Objects.requireNonNull(handler, "handler")) != null) {
            throw new IllegalStateException("request id was reused");
        }
        return requestId;
    }

    ByteString replace(ByteString previousRequestId, RejectionHandler handler) {
        if (!previousRequestId.isEmpty()) {
            handlers.remove(previousRequestId);
        }
        return track(handler);
    }

    RejectionHandler take(ByteString requestId) {
        return handlers.remove(requestId);
    }

    void complete(ByteString requestId) {
        handlers.remove(requestId);
    }

    void clear() {
        handlers.clear();
    }

    interface RejectionHandler {
        void reject(CommandRejectedException failure);
    }
}
