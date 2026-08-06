package io.github.theking90000.mumbleserverruntime.controller;

import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.ThreadFactory;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;

interface SessionScheduler {
    Cancellable schedule(Runnable task, long delayMillis);

    void close();

    interface Cancellable {
        void cancel();
    }

    final class Default implements SessionScheduler {
        private static final AtomicInteger THREAD_SEQUENCE = new AtomicInteger();
        private final ScheduledExecutorService executor = Executors.newSingleThreadScheduledExecutor(
                new ThreadFactory() {
                    @Override
                    public Thread newThread(Runnable task) {
                        Thread thread = new Thread(
                                task,
                                "controller-session-" + THREAD_SEQUENCE.incrementAndGet());
                        thread.setDaemon(true);
                        return thread;
                    }
                });

        @Override
        public Cancellable schedule(Runnable task, long delayMillis) {
            final ScheduledFuture<?> future = executor.schedule(task, delayMillis, TimeUnit.MILLISECONDS);
            return new Cancellable() {
                @Override
                public void cancel() {
                    future.cancel(false);
                }
            };
        }

        @Override
        public void close() {
            executor.shutdownNow();
        }
    }
}
