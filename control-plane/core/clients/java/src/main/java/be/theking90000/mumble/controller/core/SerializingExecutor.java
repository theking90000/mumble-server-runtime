package be.theking90000.mumble.controller.core;

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
            boolean submitted = false;
            try {
                delegate.execute(new Runnable() {
                    @Override
                    public void run() {
                        drain();
                    }
                });
                submitted = true;
            } catch (RuntimeException failure) {
                // A delegate that refuses the drain must not leave the queue claimed forever;
                // the queued tasks stay pending and the next execute() schedules a fresh drain.
                report(failure);
            } finally {
                if (!submitted) {
                    release();
                }
            }
        }
    }

    private void drain() {
        boolean drained = false;
        try {
            while (true) {
                Runnable task;
                synchronized (tasks) {
                    task = tasks.poll();
                    if (task == null) {
                        running = false;
                        drained = true;
                        return;
                    }
                }
                try {
                    task.run();
                } catch (RuntimeException failure) {
                    report(failure);
                }
            }
        } finally {
            // An Error thrown by a listener escapes to the delegate's thread, but it may not
            // strand the queue in the claimed state.
            if (!drained) {
                release();
            }
        }
    }

    private void release() {
        synchronized (tasks) {
            running = false;
        }
    }

    private static void report(Throwable failure) {
        Thread current = Thread.currentThread();
        current.getUncaughtExceptionHandler().uncaughtException(current, failure);
    }
}
