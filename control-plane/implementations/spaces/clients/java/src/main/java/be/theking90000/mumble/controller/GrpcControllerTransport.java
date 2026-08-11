package be.theking90000.mumble.controller;

import be.theking90000.mumble.controller.internal.protocol.v1.ClientFrame;
import java.net.URI;
import java.util.concurrent.CompletableFuture;

/** Spaces codec adapter over the generic gRPC Core transport. */
final class GrpcControllerTransport implements ControllerTransport {
    private final CoreTransport core;

    GrpcControllerTransport(URI endpoint, TlsConfig tlsConfig) {
        core = new GrpcCoreTransport(endpoint, tlsConfig);
    }

    @Override
    public void connect(final Listener listener) {
        core.connect(new CoreTransport.Listener() {
            @Override
            public void onConnected() {
                listener.onConnected();
            }

            @Override
            public void onFrame(
                    be.theking90000.mumble.controller.internal.core.v1.ServerFrame frame) {
                listener.onFrame(WireAdapter.fromCore(frame));
            }

            @Override
            public void onClosed(Throwable failure, boolean retryable) {
                listener.onClosed(failure, retryable);
            }
        });
    }

    @Override
    public CompletableFuture<Void> send(ClientFrame frame) {
        return core.send(WireAdapter.toCore(frame));
    }

    @Override
    public void close() {
        core.close();
    }
}
