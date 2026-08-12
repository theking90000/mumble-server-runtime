package be.theking90000.mumble.controller.spaces.load;

import java.io.PrintWriter;
import java.util.ArrayList;
import java.util.Iterator;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ArrayBlockingQueue;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicLong;

final class DriverOutput {
    private final ArrayBlockingQueue<String> critical;
    private final ConcurrentHashMap<String, Map<String, ?>> latest =
            new ConcurrentHashMap<String, Map<String, ?>>();
    private final int latestCapacity;
    private final int batchSize;
    private final PrintWriter writer;
    private final AtomicBoolean closing = new AtomicBoolean();
    private final AtomicBoolean failed = new AtomicBoolean();
    private final AtomicLong enqueued = new AtomicLong();
    private final AtomicLong coalesced = new AtomicLong();
    private final AtomicLong suppressed = new AtomicLong();
    private final AtomicLong written = new AtomicLong();
    private final AtomicLong batches = new AtomicLong();
    private final Thread thread;

    DriverOutput(PrintWriter writer, int capacity, int batchSize) {
        if (capacity < 1 || batchSize < 1) {
            throw new IllegalArgumentException("output capacities must be positive");
        }
        this.writer = writer;
        this.critical = new ArrayBlockingQueue<String>(capacity);
        this.latestCapacity = capacity;
        this.batchSize = batchSize;
        this.thread = new Thread(new Runnable() {
            @Override
            public void run() {
                writeOutput();
            }
        }, "spaces-load-driver-output");
        this.thread.setDaemon(true);
    }

    void start() {
        thread.start();
    }

    void emit(String line) {
        if (critical.offer(line)) {
            enqueued.incrementAndGet();
        } else {
            failed.set(true);
        }
    }

    void emitLatest(String key, Map<String, ?> event) {
        if (!latest.containsKey(key) && latest.size() >= latestCapacity) {
            failed.set(true);
            return;
        }
        if (latest.put(key, event) != null) {
            coalesced.incrementAndGet();
        } else {
            enqueued.incrementAndGet();
        }
    }

    void suppress() {
        suppressed.incrementAndGet();
    }

    Snapshot snapshot() {
        return new Snapshot(
                enqueued.get(),
                coalesced.get(),
                suppressed.get(),
                written.get(),
                batches.get());
    }

    boolean failed() {
        return failed.get();
    }

    void close(long timeoutMillis) throws InterruptedException {
        closing.set(true);
        thread.interrupt();
        thread.join(timeoutMillis);
        if (thread.isAlive()) {
            failed.set(true);
        }
    }

    private void writeOutput() {
        List<String> batch = new ArrayList<String>(batchSize);
        while (!closing.get() || !critical.isEmpty() || !latest.isEmpty()) {
            batch.clear();
            try {
                String first = critical.poll(50, TimeUnit.MILLISECONDS);
                if (first != null) {
                    batch.add(first);
                }
            } catch (InterruptedException interrupted) {
                if (!closing.get()) {
                    Thread.currentThread().interrupt();
                    failed.set(true);
                    return;
                }
            }
            critical.drainTo(batch, batchSize - batch.size());
            drainLatest(batch);
            if (batch.isEmpty()) {
                continue;
            }
            for (String line : batch) {
                writer.println(line);
            }
            writer.flush();
            batches.incrementAndGet();
            written.addAndGet(batch.size());
            if (writer.checkError()) {
                failed.set(true);
                return;
            }
        }
    }

    private void drainLatest(List<String> batch) {
        Iterator<Map.Entry<String, Map<String, ?>>> entries = latest.entrySet().iterator();
        while (batch.size() < batchSize && entries.hasNext()) {
            Map.Entry<String, Map<String, ?>> entry = entries.next();
            if (latest.remove(entry.getKey(), entry.getValue())) {
                batch.add(JsonLine.object(entry.getValue()));
            }
        }
    }

    static final class Snapshot {
        final long enqueued;
        final long coalesced;
        final long suppressed;
        final long written;
        final long batches;

        Snapshot(long enqueued, long coalesced, long suppressed, long written, long batches) {
            this.enqueued = enqueued;
            this.coalesced = coalesced;
            this.suppressed = suppressed;
            this.written = written;
            this.batches = batches;
        }
    }
}
