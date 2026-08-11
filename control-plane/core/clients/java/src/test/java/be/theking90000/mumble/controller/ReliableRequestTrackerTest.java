package be.theking90000.mumble.controller;

import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;

import com.google.protobuf.ByteString;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.function.Supplier;
import org.junit.jupiter.api.Test;

final class ReliableRequestTrackerTest {
    @Test
    void replacing_a_session_scoped_request_retires_its_predecessor() {
        AtomicInteger sequence = new AtomicInteger();
        ReliableRequestTracker tracker = new ReliableRequestTracker(new Supplier<ByteString>() {
            @Override
            public ByteString get() {
                return ByteString.copyFromUtf8(Integer.toString(sequence.incrementAndGet()));
            }
        });
        ReliableRequestTracker.RejectionHandler handler = failure -> { };

        ByteString first = tracker.track(handler);
        ByteString second = tracker.replace(first, handler);

        assertNull(tracker.take(first));
        assertNotNull(tracker.take(second));
    }

    @Test
    void duplicate_request_ids_fail_closed() {
        ReliableRequestTracker tracker = new ReliableRequestTracker(
                () -> ByteString.copyFromUtf8("same"));
        ReliableRequestTracker.RejectionHandler handler = failure -> { };
        tracker.track(handler);

        assertThrows(IllegalStateException.class, () -> tracker.track(handler));
    }
}
