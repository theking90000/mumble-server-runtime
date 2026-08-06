package be.theking90000.mumble.controller;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.concurrent.Executor;
import java.util.concurrent.RejectedExecutionException;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

final class SerializingExecutorTest {
    @Test
    void aRejectedDrainDoesNotWedgeTheQueue() {
        RejectingExecutor delegate = new RejectingExecutor(1);
        SerializingExecutor executor = new SerializingExecutor(delegate);
        final List<String> delivered = new ArrayList<String>();

        executor.execute(new Appender(delivered, "first"));
        assertTrue(delivered.isEmpty());

        executor.execute(new Appender(delivered, "second"));

        assertEquals(Arrays.asList("first", "second"), delivered);
    }

    @Test
    void anErrorFromATaskDoesNotWedgeTheQueue() {
        SerializingExecutor executor = new SerializingExecutor(new InlineExecutor());
        final List<String> delivered = new ArrayList<String>();

        assertThrows(AssertionError.class, new org.junit.jupiter.api.function.Executable() {
            @Override
            public void execute() {
                executor.execute(new Runnable() {
                    @Override
                    public void run() {
                        throw new AssertionError("listener failure");
                    }
                });
            }
        });

        executor.execute(new Appender(delivered, "after"));

        assertEquals(Arrays.asList("after"), delivered);
    }

    private static final class Appender implements Runnable {
        private final List<String> target;
        private final String value;

        private Appender(List<String> target, String value) {
            this.target = target;
            this.value = value;
        }

        @Override
        public void run() {
            target.add(value);
        }
    }

    private static final class InlineExecutor implements Executor {
        @Override
        public void execute(Runnable command) {
            command.run();
        }
    }

    private static final class RejectingExecutor implements Executor {
        private int remainingRejections;

        private RejectingExecutor(int remainingRejections) {
            this.remainingRejections = remainingRejections;
        }

        @Override
        public void execute(Runnable command) {
            if (remainingRejections > 0) {
                remainingRejections--;
                throw new RejectedExecutionException("scripted rejection");
            }
            command.run();
        }
    }
}
