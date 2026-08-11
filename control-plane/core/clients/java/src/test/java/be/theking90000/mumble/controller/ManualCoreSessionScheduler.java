package be.theking90000.mumble.controller;

import java.util.ArrayDeque;
import java.util.Queue;

final class ManualCoreSessionScheduler implements SessionScheduler {
    private final Queue<Task> tasks = new ArrayDeque<Task>();

    @Override
    public Cancellable schedule(Runnable runnable, long delayMillis) {
        final Task task = new Task(runnable);
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
        tasks.clear();
    }

    void runNext() {
        Task task = nextLiveTask();
        tasks.remove(task);
        task.runnable.run();
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
        private boolean cancelled;

        private Task(Runnable runnable) {
            this.runnable = runnable;
        }
    }
}
