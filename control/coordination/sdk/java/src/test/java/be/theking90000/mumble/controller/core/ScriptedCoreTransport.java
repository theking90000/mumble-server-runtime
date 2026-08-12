package be.theking90000.mumble.controller.core;

import be.theking90000.mumble.controller.internal.core.v1.ClientFrame;
import be.theking90000.mumble.controller.internal.core.v1.ServerFrame;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.concurrent.CompletableFuture;

final class ScriptedCoreTransport implements CoreTransport {
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
            failed.completeExceptionally(new ControllerException("scripted transport is closed"));
            return failed;
        }
        sent.add(frame);
        return CompletableFuture.completedFuture(null);
    }

    @Override
    public void close() {
        closed = true;
    }

    List<ClientFrame> sent() {
        return Collections.unmodifiableList(sent);
    }

    ClientFrame lastSent() {
        if (sent.isEmpty()) {
            throw new AssertionError("no client frame was sent");
        }
        return sent.get(sent.size() - 1);
    }

    void emit(ServerFrame frame) {
        listener.onFrame(frame);
    }

    void disconnect(boolean retryable) {
        listener.onClosed(new ControllerException("scripted disconnect"), retryable);
    }

    static final class Factory implements CoreTransport.Factory {
        private final List<ScriptedCoreTransport> transports =
                new ArrayList<ScriptedCoreTransport>();

        @Override
        public CoreTransport create() {
            ScriptedCoreTransport transport = new ScriptedCoreTransport();
            transports.add(transport);
            return transport;
        }

        ScriptedCoreTransport current() {
            if (transports.isEmpty()) {
                throw new AssertionError("no transport was created");
            }
            return transports.get(transports.size() - 1);
        }
    }
}
