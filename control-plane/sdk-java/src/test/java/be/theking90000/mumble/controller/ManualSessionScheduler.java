package be.theking90000.mumble.controller;

import java.util.ArrayDeque;
import java.util.Queue;

final class ManualSessionScheduler implements SessionScheduler {
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

    long nextDelayMillis() {
        Task task = nextLiveTask();
        return task.delayMillis;
    }

    void runNext() {
        Task task = nextLiveTask();
        tasks.remove(task);
        task.runnable.run();
    }

    boolean isClosed() {
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
