package be.theking90000.mumble.controller.core;

import be.theking90000.mumble.controller.internal.core.v1.ClientFrame;
import be.theking90000.mumble.controller.internal.core.v1.ServerFrame;
import com.google.protobuf.ByteString;
import java.net.URI;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Queue;
import java.util.Random;
import java.util.concurrent.CompletableFuture;

/** Package-local Core access collected behind one test-only boundary for profile facade tests. */
public final class CoreSessionTestkit {
    private CoreSessionTestkit() {
    }

    /** Builds a Core session backed by deterministic test transport and scheduling. */
    public static CoreSession session(
            ControllerId controllerId,
            URI endpoint,
            ProfileReference profile,
            Factory transports,
            Scheduler scheduler) {
        return CoreSession.builder(controllerId, endpoint, profile)
                .callbackExecutor(Runnable::run)
                .transportFactory(transports)
                .scheduler(scheduler)
                .random(new Random(0L))
                .build();
    }

    /** Returns the opaque registration identity for protocol assertions. */
    public static ByteString registrationId(CoreParticipantHandle participant) {
        return participant.registrationId();
    }

    /** Returns the current client specification revision for protocol assertions. */
    public static long clientSpecRevision(CoreParticipantHandle participant) {
        return participant.clientSpecRevision();
    }

    /** Scripted transport factory retained by a test across reconnects. */
    public static final class Factory implements CoreTransport.Factory {
        private final List<Transport> transports = new ArrayList<Transport>();

        @Override
        public CoreTransport create() {
            Transport transport = new Transport();
            transports.add(transport);
            return transport;
        }

        /** @return most recently created transport */
        public Transport current() {
            if (transports.isEmpty()) {
                throw new AssertionError("no transport was created");
            }
            return transports.get(transports.size() - 1);
        }

        /** @return number of transports created across reconnects */
        public int createdCount() {
            return transports.size();
        }
    }

    /** One scripted Core stream. */
    public static final class Transport implements CoreTransport {
        private final List<ClientFrame> sent = new ArrayList<ClientFrame>();
        private Listener listener;
        private boolean closed;

        @Override
        public void connect(Listener value) {
            listener = value;
            value.onConnected();
        }

        @Override
        public CompletableFuture<Void> send(ClientFrame frame) {
            if (closed) {
                CompletableFuture<Void> failed = new CompletableFuture<Void>();
                failed.completeExceptionally(
                        new ControllerException("scripted transport is closed"));
                return failed;
            }
            sent.add(frame);
            return CompletableFuture.completedFuture(null);
        }

        @Override
        public void close() {
            closed = true;
        }

        /** @return immutable list of frames sent on this stream */
        public List<ClientFrame> sent() {
            return Collections.unmodifiableList(sent);
        }

        /** @return most recently sent frame */
        public ClientFrame lastSent() {
            if (sent.isEmpty()) {
                throw new AssertionError("no client frame was sent");
            }
            return sent.get(sent.size() - 1);
        }

        /** Emits one server frame. */
        public void emit(ServerFrame frame) {
            listener.onFrame(frame);
        }

        /** Closes this stream from the remote side. */
        public void disconnect(boolean retryable) {
            listener.onClosed(new ControllerException("scripted disconnect"), retryable);
        }

        /** Replays a late connected callback from this stream. */
        public void signalConnected() {
            listener.onConnected();
        }
    }

    /** Deterministic scheduler used by facade tests. */
    public static final class Scheduler implements SessionScheduler {
        private final Queue<Task> tasks = new ArrayDeque<Task>();
        private boolean closed;

        @Override
        public Cancellable schedule(Runnable runnable, long delayMillis) {
            final Task task = new Task(runnable, delayMillis);
            tasks.add(task);
            return new Cancellable() {
                @Override
                public void cancel() {
                    task.cancelled = true;
                }
            };
        }

        @Override
        public void close() {
            closed = true;
            tasks.clear();
        }

        /** @return delay of the next live task */
        public long nextDelayMillis() {
            return nextLiveTask().delayMillis;
        }

        /** Runs the next live task immediately. */
        public void runNext() {
            Task task = nextLiveTask();
            tasks.remove(task);
            task.runnable.run();
        }

        /** @return whether Core closed this scheduler */
        public boolean isClosed() {
            return closed;
        }

        private Task nextLiveTask() {
            while (!tasks.isEmpty() && tasks.peek().cancelled) {
                tasks.remove();
            }
            Task task = tasks.peek();
            if (task == null) {
                throw new AssertionError("no scheduled task");
            }
            return task;
        }

        private static final class Task {
            private final Runnable runnable;
            private final long delayMillis;
            private boolean cancelled;

            private Task(Runnable runnable, long delayMillis) {
                this.runnable = runnable;
                this.delayMillis = delayMillis;
            }
        }
    }
}
