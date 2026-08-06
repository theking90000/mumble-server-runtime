package io.github.theking90000.mumbleserverruntime.controller;

import java.util.ArrayDeque;
import java.util.Queue;
import java.util.concurrent.Executor;

final class SerializingExecutor implements Executor {
    private final Executor delegate;
    private final Queue<Runnable> tasks = new ArrayDeque<Runnable>();
    private boolean running;

    SerializingExecutor(Executor delegate) {
        this.delegate = delegate;
    }

    @Override
    public void execute(Runnable command) {
        boolean schedule;
        synchronized (tasks) {
            tasks.add(command);
            schedule = !running;
            if (schedule) {
                running = true;
            }
        }
        if (schedule) {
            delegate.execute(new Runnable() {
                @Override
                public void run() {
                    drain();
                }
            });
        }
    }

    private void drain() {
        while (true) {
            Runnable task;
            synchronized (tasks) {
                task = tasks.poll();
                if (task == null) {
                    running = false;
                    return;
                }
            }
            try {
                task.run();
            } catch (RuntimeException failure) {
                Thread current = Thread.currentThread();
                current.getUncaughtExceptionHandler().uncaughtException(current, failure);
            }
        }
    }
}
