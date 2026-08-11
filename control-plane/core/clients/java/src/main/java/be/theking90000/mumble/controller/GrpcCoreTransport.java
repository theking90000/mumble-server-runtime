package be.theking90000.mumble.controller;

import be.theking90000.mumble.controller.internal.core.v1.ClientFrame;
import be.theking90000.mumble.controller.internal.core.v1.ControllerServiceGrpc;
import be.theking90000.mumble.controller.internal.core.v1.ServerFrame;
import io.grpc.ManagedChannel;
import io.grpc.Status;
import io.grpc.netty.shaded.io.grpc.netty.GrpcSslContexts;
import io.grpc.netty.shaded.io.grpc.netty.NettyChannelBuilder;
import io.grpc.stub.StreamObserver;
import java.io.File;
import java.net.URI;
import java.util.Objects;
import java.util.concurrent.CompletableFuture;
import javax.net.ssl.SSLException;

/** gRPC implementation of the generic Controller Core transport. */
final class GrpcCoreTransport implements CoreTransport {
    private final URI endpoint;
    private final TlsConfig tlsConfig;
    private final Object lock = new Object();
    private ManagedChannel channel;
    private StreamObserver<ClientFrame> requestObserver;
    private Listener listener;
    private boolean closed;
    private boolean terminated;

    GrpcCoreTransport(URI endpoint, TlsConfig tlsConfig) {
        this.endpoint = Objects.requireNonNull(endpoint, "endpoint");
        this.tlsConfig = tlsConfig;
    }

    @Override
    public void connect(Listener value) {
        Objects.requireNonNull(value, "listener");
        final ManagedChannel createdChannel;
        synchronized (lock) {
            if (listener != null) {
                throw new IllegalStateException("transport instances connect only once");
            }
            listener = value;
            channel = buildChannel();
            createdChannel = channel;
        }

        StreamObserver<ServerFrame> responses = new StreamObserver<ServerFrame>() {
            @Override
            public void onNext(ServerFrame frame) {
                value.onFrame(frame);
            }

            @Override
            public void onError(Throwable failure) {
                if (terminate()) {
                    value.onClosed(failure, isRetryable(failure));
                }
            }

            @Override
            public void onCompleted() {
                if (terminate()) {
                    value.onClosed(new ControllerException("controller stream closed by peer"), true);
                }
            }
        };

        StreamObserver<ClientFrame> requests = ControllerServiceGrpc
                .newStub(createdChannel)
                .connect(responses);
        synchronized (lock) {
            if (terminated) {
                createdChannel.shutdownNow();
                return;
            }
            requestObserver = requests;
        }
        value.onConnected();
    }

    @Override
    public CompletableFuture<Void> send(ClientFrame frame) {
        Objects.requireNonNull(frame, "frame");
        synchronized (lock) {
            if (closed || requestObserver == null) {
                return failedFuture(new ControllerException("controller transport is not connected"));
            }
            try {
                requestObserver.onNext(frame);
                return CompletableFuture.completedFuture(null);
            } catch (RuntimeException failure) {
                return failedFuture(new ControllerException("failed to send controller frame", failure));
            }
        }
    }

    @Override
    public void close() {
        synchronized (lock) {
            if (closed) {
                return;
            }
            closed = true;
            terminated = true;
            if (requestObserver != null) {
                requestObserver.onCompleted();
            }
            if (channel != null) {
                channel.shutdownNow();
            }
        }
    }

    private boolean terminate() {
        synchronized (lock) {
            if (terminated) {
                return false;
            }
            terminated = true;
            return true;
        }
    }

    private ManagedChannel buildChannel() {
        int port = endpoint.getPort();
        if (port < 0) {
            port = tlsConfig == null ? 80 : 443;
        }
        NettyChannelBuilder builder = NettyChannelBuilder.forAddress(endpoint.getHost(), port);
        if (tlsConfig == null) {
            return builder.usePlaintext().build();
        }
        if (!tlsConfig.trustCertificate().isPresent()) {
            return builder.useTransportSecurity().build();
        }
        File certificate = tlsConfig.trustCertificate().get().toFile();
        try {
            return builder.sslContext(GrpcSslContexts.forClient().trustManager(certificate).build()).build();
        } catch (SSLException failure) {
            throw new ControllerException("invalid controller TLS configuration", failure);
        }
    }

    private static boolean isRetryable(Throwable failure) {
        Status.Code code = Status.fromThrowable(failure).getCode();
        return code == Status.Code.UNAVAILABLE
                || code == Status.Code.CANCELLED
                || code == Status.Code.DEADLINE_EXCEEDED
                || code == Status.Code.UNKNOWN;
    }

    private static <T> CompletableFuture<T> failedFuture(Throwable failure) {
        CompletableFuture<T> future = new CompletableFuture<T>();
        future.completeExceptionally(failure);
        return future;
    }
}
