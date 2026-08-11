package be.theking90000.mumble.controller;

import be.theking90000.mumble.controller.internal.core.v1.ClientFrame;
import be.theking90000.mumble.controller.internal.core.v1.ServerFrame;
import java.util.concurrent.CompletableFuture;

/** Internal transport boundary shared by typed Controller profiles. */
interface CoreTransport {
    void connect(Listener listener);

    CompletableFuture<Void> send(ClientFrame frame);

    void close();

    /** Receives serialized lifecycle callbacks from the Core transport. */
    interface Listener {
        void onConnected();

        void onFrame(ServerFrame frame);

        void onClosed(Throwable failure, boolean retryable);
    }

    interface Factory {
        CoreTransport create();
    }
}
