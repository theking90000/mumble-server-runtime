package io.github.theking90000.mumbleserverruntime.controller;

import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.ClientFrame;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.ServerFrame;
import java.util.concurrent.CompletableFuture;

interface ControllerTransport {
    void connect(Listener listener);

    CompletableFuture<Void> send(ClientFrame frame);

    void close();

    interface Listener {
        void onConnected();

        void onFrame(ServerFrame frame);

        void onClosed(Throwable failure, boolean retryable);
    }

    interface Factory {
        ControllerTransport create();
    }
}
