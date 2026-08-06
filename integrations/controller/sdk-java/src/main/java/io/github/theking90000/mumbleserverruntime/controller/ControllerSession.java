package io.github.theking90000.mumbleserverruntime.controller;

import com.google.protobuf.ByteString;
import com.google.protobuf.Duration;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.ClientFrame;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.CloseSession;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.CommandRejected;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.DesiredStateReconciled;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.DesiredStateSnapshot;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.FetchSpace;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.FetchSpaceResult;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.OpenSession;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.ObservedSpacesAccepted;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.ParticipantOwnershipGranted;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.ParticipantOwnershipRevoked;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.ParticipantRegistration;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.ParticipantSpecAccepted;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.ParticipantStatusChanged;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.RegisterParticipant;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.ReleaseParticipant;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.RenewLease;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.ReplaceObservedSpaces;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.ServerFrame;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.SessionReady;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.SetParticipantSpec;
import io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.SyncDesiredState;
import java.net.URI;
import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Comparator;
import java.util.HashMap;
import java.util.Iterator;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.NavigableMap;
import java.util.Objects;
import java.util.Optional;
import java.util.Random;
import java.util.Set;
import java.util.TreeMap;
import java.util.TreeSet;
import java.util.UUID;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.Executor;
import java.util.concurrent.ForkJoinPool;

/**
 * Thread-safe owner of a controller's desired participant state and read-only space cache.
 *
 * <p>The session reconnects and reconciles automatically. A transport acknowledgement is
 * intentionally distinct from accepted, applied, and published state.</p>
 */
public final class ControllerSession {
    private static final long INITIAL_RECONNECT_MILLIS = 250L;
    private static final long MAX_RECONNECT_MILLIS = 10_000L;
    private static final double RECONNECT_JITTER = 0.20d;

    private final Object monitor = new Object();
    private final ControllerId controllerId;
    private final ByteString controllerInstanceId;
    private final ControllerTransport.Factory transportFactory;
    private final SessionScheduler scheduler;
    private final SerializingExecutor callbackExecutor;
    private final Random random;
    private final Map<ParticipantId, ParticipantHandle> participants =
            new LinkedHashMap<ParticipantId, ParticipantHandle>();
    private final Set<SpaceKey> explicitlyObservedSpaces = new TreeSet<SpaceKey>();
    private final Map<SpaceKey, SpaceSnapshot> spaces = new LinkedHashMap<SpaceKey, SpaceSnapshot>();
    private final CopyOnWriteArrayList<ControllerSessionListener> sessionListeners =
            new CopyOnWriteArrayList<ControllerSessionListener>();
    private final CopyOnWriteArrayList<SpaceListener> spaceListeners =
            new CopyOnWriteArrayList<SpaceListener>();
    private final NavigableMap<Long, List<CompletableFuture<Void>>> pendingObservedSpaces =
            new TreeMap<Long, List<CompletableFuture<Void>>>();
    private final Map<ByteString, CompletableFuture<SpaceSnapshot>> pendingFetches =
            new HashMap<ByteString, CompletableFuture<SpaceSnapshot>>();
    private final Map<ByteString, RejectionHandler> rejectionHandlers =
            new HashMap<ByteString, RejectionHandler>();
    private final CompletableFuture<Void> startFuture = new CompletableFuture<Void>();
    private final CompletableFuture<Void> stopFuture = new CompletableFuture<Void>();

    private ControllerSessionState state = ControllerSessionState.NEW;
    private ControllerTransport transport;
    private ByteString resumeToken = ByteString.EMPTY;
    private ByteString sessionToken = ByteString.EMPTY;
    private long desiredStateRevision;
    private long observedSpacesRevision;
    private long reconciledStateRevision;
    private boolean sessionReady;
    private boolean reconciliationReceived;
    private int reconnectAttempt;
    private SessionScheduler.Cancellable reconnectTask;
    private SessionScheduler.Cancellable leaseTask;

    private ControllerSession(Builder builder) {
        controllerId = builder.controllerId;
        controllerInstanceId = randomIdentifier();
        callbackExecutor = new SerializingExecutor(builder.callbackExecutor);
        scheduler = builder.scheduler == null ? new SessionScheduler.Default() : builder.scheduler;
        random = builder.random == null ? new Random() : builder.random;
        if (builder.transportFactory == null) {
            final URI endpoint = builder.endpoint;
            final TlsConfig tlsConfig = builder.tlsConfig;
            transportFactory = new ControllerTransport.Factory() {
                @Override
                public ControllerTransport create() {
                    return new GrpcControllerTransport(endpoint, tlsConfig);
                }
            };
        } else {
            transportFactory = builder.transportFactory;
        }
    }

    public static Builder builder(ControllerId controllerId, URI endpoint) {
        return new Builder(controllerId, endpoint);
    }

    public ControllerId controllerId() {
        return controllerId;
    }

    public ControllerSessionState state() {
        synchronized (monitor) {
            return state;
        }
    }

    public CompletableFuture<Void> start() {
        synchronized (monitor) {
            if (state != ControllerSessionState.NEW) {
                return startFuture;
            }
            changeState(ControllerSessionState.CONNECTING);
            connectNow();
            return startFuture;
        }
    }

    public ParticipantHandle registerParticipant(ParticipantId participantId, ParticipantSpec spec) {
        Objects.requireNonNull(participantId, "participantId");
        Objects.requireNonNull(spec, "spec");
        synchronized (monitor) {
            ensureNotTerminal();
            ParticipantHandle existing = participants.get(participantId);
            if (existing != null) {
                if (existing.isDesired()
                        || existing.unregisterFuture() == null
                        || !existing.unregisterFuture().isDone()) {
                    throw new IllegalStateException(
                            "participant registration or release is already in progress: " + participantId);
                }
                participants.remove(participantId);
            }
            ParticipantHandle handle = new ParticipantHandle(
                    this,
                    participantId,
                    randomIdentifier(),
                    spec);
            participants.put(participantId, handle);
            desiredStateRevision++;
            if (state == ControllerSessionState.ACTIVE) {
                sendRegister(handle);
            }
            return handle;
        }
    }

    public Optional<ParticipantHandle> participant(ParticipantId participantId) {
        Objects.requireNonNull(participantId, "participantId");
        synchronized (monitor) {
            ParticipantHandle handle = participants.get(participantId);
            return handle != null && handle.isDesired()
                    ? Optional.of(handle)
                    : Optional.<ParticipantHandle>empty();
        }
    }

    public Map<ParticipantId, ParticipantHandle> participants() {
        synchronized (monitor) {
            Map<ParticipantId, ParticipantHandle> snapshot =
                    new LinkedHashMap<ParticipantId, ParticipantHandle>();
            for (Map.Entry<ParticipantId, ParticipantHandle> entry : participants.entrySet()) {
                if (entry.getValue().isDesired()) {
                    snapshot.put(entry.getKey(), entry.getValue());
                }
            }
            return Collections.unmodifiableMap(snapshot);
        }
    }

    public CompletableFuture<Void> observeSpace(SpaceKey spaceKey) {
        return setSpaceObservation(Objects.requireNonNull(spaceKey, "spaceKey"), true);
    }

    public CompletableFuture<Void> unobserveSpace(SpaceKey spaceKey) {
        return setSpaceObservation(Objects.requireNonNull(spaceKey, "spaceKey"), false);
    }

    public Set<SpaceKey> explicitlyObservedSpaces() {
        synchronized (monitor) {
            return Collections.unmodifiableSet(new TreeSet<SpaceKey>(explicitlyObservedSpaces));
        }
    }

    public Map<SpaceKey, SpaceSnapshot> spaces() {
        synchronized (monitor) {
            return Collections.unmodifiableMap(new LinkedHashMap<SpaceKey, SpaceSnapshot>(spaces));
        }
    }

    public CompletableFuture<SpaceSnapshot> fetchSpace(SpaceKey spaceKey) {
        Objects.requireNonNull(spaceKey, "spaceKey");
        synchronized (monitor) {
            ensureActive();
            final CompletableFuture<SpaceSnapshot> future = new CompletableFuture<SpaceSnapshot>();
            ByteString requestId = nextRequestId();
            pendingFetches.put(requestId, future);
            rejectionHandlers.put(requestId, new RejectionHandler() {
                @Override
                public void reject(CommandRejectedException failure) {
                    future.completeExceptionally(failure);
                }
            });
            FetchSpace command = FetchSpace.newBuilder()
                    .setSessionToken(sessionToken)
                    .setSpaceKey(spaceKey.value())
                    .build();
            sendFrame(ClientFrame.newBuilder()
                    .setRequestId(requestId)
                    .setFetchSpace(command)
                    .build());
            return future;
        }
    }

    public CompletableFuture<Void> stop() {
        synchronized (monitor) {
            if (state == ControllerSessionState.CLOSED) {
                return stopFuture;
            }
            if (state == ControllerSessionState.NEW || state == ControllerSessionState.FAILED) {
                closeLocally();
                return stopFuture;
            }
            if (state == ControllerSessionState.STOPPING) {
                return stopFuture;
            }
            changeState(ControllerSessionState.STOPPING);
            cancelScheduledTasks();
            if (transport == null || sessionToken.isEmpty()) {
                closeLocally();
                return stopFuture;
            }
            ByteString requestId = nextRequestId();
            rejectionHandlers.put(requestId, new RejectionHandler() {
                @Override
                public void reject(CommandRejectedException failure) {
                    closeLocally();
                }
            });
            sendFrame(ClientFrame.newBuilder()
                    .setRequestId(requestId)
                    .setCloseSession(CloseSession.newBuilder().setSessionToken(sessionToken).build())
                    .build());
            return stopFuture;
        }
    }

    public void addSessionListener(ControllerSessionListener listener) {
        sessionListeners.add(Objects.requireNonNull(listener, "listener"));
    }

    public void removeSessionListener(ControllerSessionListener listener) {
        sessionListeners.remove(listener);
    }

    public void addSpaceListener(SpaceListener listener) {
        spaceListeners.add(Objects.requireNonNull(listener, "listener"));
    }

    public void removeSpaceListener(SpaceListener listener) {
        spaceListeners.remove(listener);
    }

    Object monitor() {
        return monitor;
    }

    void dispatch(Runnable callback) {
        callbackExecutor.execute(callback);
    }

    CompletableFuture<AcceptedRevision> setParticipantSpec(
            final ParticipantHandle handle,
            ParticipantSpec spec) {
        synchronized (monitor) {
            requireCurrentHandle(handle);
            CompletableFuture<AcceptedRevision> future = handle.replaceDesiredSpec(spec);
            desiredStateRevision++;
            if (state == ControllerSessionState.ACTIVE
                    && handle.state() == ParticipantHandleState.OWNED) {
                sendSpec(handle, handle.clientSpecRevision());
            }
            return future;
        }
    }

    CompletableFuture<Void> unregisterParticipant(final ParticipantHandle handle) {
        synchronized (monitor) {
            requireCurrentHandle(handle);
            CompletableFuture<Void> future = handle.beginUnregister();
            desiredStateRevision++;
            if (handle.ownershipToken().isEmpty()) {
                if (state == ControllerSessionState.NEW) {
                    handle.releaseAcknowledged();
                    participants.remove(handle.participantId());
                }
                return future;
            }
            if (state == ControllerSessionState.ACTIVE) {
                sendRelease(handle);
            }
            return future;
        }
    }

    private CompletableFuture<Void> setSpaceObservation(SpaceKey spaceKey, boolean observe) {
        synchronized (monitor) {
            ensureNotTerminal();
            boolean changed = observe
                    ? explicitlyObservedSpaces.add(spaceKey)
                    : explicitlyObservedSpaces.remove(spaceKey);
            if (!changed) {
                List<CompletableFuture<Void>> pending =
                        pendingObservedSpaces.get(observedSpacesRevision);
                if (pending != null && !pending.isEmpty()) {
                    return pending.get(pending.size() - 1);
                }
                return CompletableFuture.completedFuture(null);
            }
            observedSpacesRevision++;
            desiredStateRevision++;
            CompletableFuture<Void> future = new CompletableFuture<Void>();
            List<CompletableFuture<Void>> revisions = pendingObservedSpaces.get(observedSpacesRevision);
            if (revisions == null) {
                revisions = new ArrayList<CompletableFuture<Void>>();
                pendingObservedSpaces.put(observedSpacesRevision, revisions);
            }
            revisions.add(future);
            if (state == ControllerSessionState.ACTIVE) {
                sendObservedSpaces(future);
            }
            return future;
        }
    }

    private void connectNow() {
        if (state == ControllerSessionState.CLOSED
                || state == ControllerSessionState.STOPPING
                || state == ControllerSessionState.FAILED) {
            return;
        }
        try {
            transport = transportFactory.create();
            transport.connect(new ControllerTransport.Listener() {
                @Override
                public void onConnected() {
                    ControllerSession.this.onTransportConnected();
                }

                @Override
                public void onFrame(ServerFrame frame) {
                    ControllerSession.this.onServerFrame(frame);
                }

                @Override
                public void onClosed(Throwable failure, boolean retryable) {
                    ControllerSession.this.onTransportClosed(failure, retryable);
                }
            });
        } catch (RuntimeException failure) {
            handleTransportFailure(failure, true);
        }
    }

    private void onTransportConnected() {
        synchronized (monitor) {
            if (state == ControllerSessionState.STOPPING || state == ControllerSessionState.CLOSED) {
                if (transport != null) {
                    transport.close();
                }
                return;
            }
            sessionReady = false;
            reconciliationReceived = false;
            sessionToken = ByteString.EMPTY;
            changeState(ControllerSessionState.RECONCILING);
            final ByteString requestId = nextRequestId();
            rejectionHandlers.put(requestId, new RejectionHandler() {
                @Override
                public void reject(CommandRejectedException failure) {
                    failPermanently(failure);
                }
            });
            OpenSession open = OpenSession.newBuilder()
                    .setControllerId(controllerId.value())
                    .setControllerInstanceId(controllerInstanceId)
                    .setResumeToken(resumeToken)
                    .setDesiredState(buildDesiredStateSnapshot())
                    .build();
            sendFrame(ClientFrame.newBuilder()
                    .setRequestId(requestId)
                    .setOpenSession(open)
                    .build());
        }
    }

    private void onServerFrame(ServerFrame frame) {
        Objects.requireNonNull(frame, "frame");
        synchronized (monitor) {
            if (state == ControllerSessionState.CLOSED || state == ControllerSessionState.FAILED) {
                return;
            }
            switch (frame.getPayloadCase()) {
                case SESSION_READY:
                    rejectionHandlers.remove(frame.getRequestId());
                    acceptSession(frame.getSessionReady());
                    break;
                case DESIRED_STATE_RECONCILED:
                    rejectionHandlers.remove(frame.getRequestId());
                    acceptReconciliation(frame.getDesiredStateReconciled());
                    break;
                case PARTICIPANT_OWNERSHIP_GRANTED:
                    rejectionHandlers.remove(frame.getRequestId());
                    acceptOwnership(frame.getParticipantOwnershipGranted());
                    break;
                case PARTICIPANT_OWNERSHIP_REVOKED:
                    rejectionHandlers.remove(frame.getRequestId());
                    acceptRevocation(frame.getParticipantOwnershipRevoked());
                    break;
                case PARTICIPANT_SPEC_ACCEPTED:
                    rejectionHandlers.remove(frame.getRequestId());
                    acceptSpec(frame.getParticipantSpecAccepted());
                    break;
                case PARTICIPANT_STATUS_CHANGED:
                    acceptStatus(frame.getParticipantStatusChanged());
                    break;
                case OBSERVED_SPACES_ACCEPTED:
                    rejectionHandlers.remove(frame.getRequestId());
                    acceptObservedSpaces(frame.getObservedSpacesAccepted());
                    break;
                case SPACE_SNAPSHOT:
                    acceptSpaceSnapshot(frame.getSpaceSnapshot());
                    break;
                case SPACE_CLOSED:
                    acceptSpaceClosed(frame.getSpaceClosed());
                    break;
                case FETCH_SPACE_RESULT:
                    rejectionHandlers.remove(frame.getRequestId());
                    acceptFetch(frame.getRequestId(), frame.getFetchSpaceResult());
                    break;
                case RESYNC_REQUIRED:
                    beginResynchronization();
                    break;
                case COMMAND_REJECTED:
                    acceptRejection(frame.getRequestId(), frame.getCommandRejected());
                    break;
                case SESSION_CLOSING:
                    rejectionHandlers.remove(frame.getRequestId());
                    closeLocally();
                    break;
                case PAYLOAD_NOT_SET:
                default:
                    failPermanently(new ControllerException("server frame has no supported payload"));
                    break;
            }
        }
    }

    private void onTransportClosed(Throwable failure, boolean retryable) {
        synchronized (monitor) {
            handleTransportFailure(failure, retryable);
        }
    }

    private void handleTransportFailure(Throwable failure, boolean retryable) {
        if (state == ControllerSessionState.STOPPING) {
            closeLocally();
            return;
        }
        if (state == ControllerSessionState.CLOSED || state == ControllerSessionState.FAILED) {
            return;
        }
        if (!retryable) {
            failPermanently(new ControllerException("permanent controller transport failure", failure));
            return;
        }
        cancelLeaseTask();
        if (transport != null) {
            transport.close();
            transport = null;
        }
        for (ParticipantHandle handle : participants.values()) {
            handle.suspend();
        }
        changeState(ControllerSessionState.RECONNECTING);
        scheduleReconnect();
    }

    private void acceptSession(SessionReady ready) {
        if (ready.getSessionToken().isEmpty() || ready.getResumeToken().isEmpty()) {
            failPermanently(new ControllerException("SessionReady contains an empty token"));
            return;
        }
        long leaseMillis;
        try {
            leaseMillis = durationMillis(ready.getLeaseDuration());
        } catch (ArithmeticException failure) {
            failPermanently(new ControllerException("lease duration overflows milliseconds", failure));
            return;
        }
        if (leaseMillis <= 0L) {
            failPermanently(new ControllerException("lease duration must be positive"));
            return;
        }
        sessionToken = ready.getSessionToken();
        resumeToken = ready.getResumeToken();
        sessionReady = true;
        scheduleLeaseRenewal(leaseMillis);
        becomeActiveWhenReady();
    }

    private void acceptReconciliation(DesiredStateReconciled reconciled) {
        reconciledStateRevision = reconciled.getDesiredStateRevision();
        reconciliationReceived = true;
        if (Long.compareUnsigned(reconciledStateRevision, desiredStateRevision) > 0) {
            failPermanently(new ControllerException("server reconciled an unknown desired-state revision"));
            return;
        }
        removeClosedWithoutOwnership();
        if (Long.compareUnsigned(reconciledStateRevision, desiredStateRevision) < 0) {
            beginResynchronization();
            return;
        }
        becomeActiveWhenReady();
    }

    private void becomeActiveWhenReady() {
        if (!sessionReady
                || !reconciliationReceived
                || Long.compareUnsigned(reconciledStateRevision, desiredStateRevision) < 0) {
            return;
        }
        reconnectAttempt = 0;
        changeState(ControllerSessionState.ACTIVE);
        startFuture.complete(null);
        for (ParticipantHandle handle : new ArrayList<ParticipantHandle>(participants.values())) {
            if (handle.state() == ParticipantHandleState.CLOSED && !handle.ownershipToken().isEmpty()) {
                sendRelease(handle);
            } else if (handle.state() == ParticipantHandleState.OWNED
                    && Long.compareUnsigned(
                            handle.clientSpecRevision(),
                            handle.acceptedClientSpecRevision()) > 0) {
                sendSpec(handle, handle.clientSpecRevision());
            }
        }
    }

    private void acceptOwnership(ParticipantOwnershipGranted granted) {
        ParticipantId participantId = ParticipantId.of(granted.getParticipantId());
        ParticipantHandle handle = participants.get(participantId);
        if (handle == null || !handle.registrationId().equals(granted.getRegistrationId())) {
            failPermanently(new ControllerException(
                    "ownership granted for an unknown participant registration: " + participantId));
            return;
        }
        if (granted.getOwnershipToken().isEmpty()) {
            failPermanently(new ControllerException("ownership grant contains an empty token"));
            return;
        }
        handle.ownershipGranted(
                granted.getOwnershipToken(),
                granted.getClientSpecRevision(),
                granted.getAcceptedSpecRevision(),
                granted.getAppliedSpecRevision(),
                granted.getPublishedGeneration());
        if (handle.state() == ParticipantHandleState.CLOSED) {
            sendRelease(handle);
        } else if (Long.compareUnsigned(
                handle.clientSpecRevision(),
                handle.acceptedClientSpecRevision()) > 0) {
            sendSpec(handle, handle.clientSpecRevision());
        }
    }

    private void acceptRevocation(ParticipantOwnershipRevoked revoked) {
        ParticipantId participantId = ParticipantId.of(revoked.getParticipantId());
        ParticipantHandle handle = participants.get(participantId);
        if (handle == null || !handle.registrationId().equals(revoked.getRegistrationId())) {
            return;
        }
        boolean wasClosed = handle.state() == ParticipantHandleState.CLOSED;
        if (wasClosed) {
            handle.releaseAcknowledged();
        } else {
            handle.revoke(revoked.getReason().name());
        }
        participants.remove(participantId);
        if (!wasClosed) {
            desiredStateRevision++;
        }
    }

    private void acceptSpec(ParticipantSpecAccepted accepted) {
        ParticipantHandle handle = participants.get(ParticipantId.of(accepted.getParticipantId()));
        if (handle == null || !handle.isDesired()) {
            return;
        }
        handle.specAccepted(new AcceptedRevision(
                accepted.getClientSpecRevision(),
                accepted.getAcceptedSpecRevision(),
                accepted.getAppliedSpecRevision(),
                accepted.getPublishedGeneration()));
    }

    private void acceptStatus(ParticipantStatusChanged changed) {
        ParticipantHandle handle = participants.get(ParticipantId.of(changed.getParticipantId()));
        if (handle == null || !handle.isDesired()) {
            return;
        }
        io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.ParticipantStatus wire =
                changed.getStatus();
        SpaceKey appliedSpace = wire.getAppliedSpaceKey().isEmpty()
                ? null
                : SpaceKey.of(wire.getAppliedSpaceKey());
        handle.updateStatus(new ParticipantStatus(
                wire.getMumbleConnected(),
                appliedSpace,
                wire.getSelfMute(),
                wire.getSelfDeaf(),
                wire.getAcceptedSpecRevision(),
                wire.getAppliedSpecRevision(),
                wire.getPublishedGeneration(),
                wire.getApplicationError()));
    }

    private void acceptObservedSpaces(ObservedSpacesAccepted accepted) {
        if (Long.compareUnsigned(accepted.getObservedSpacesRevision(), observedSpacesRevision) > 0) {
            failPermanently(new ControllerException("server accepted an unknown observations revision"));
            return;
        }
        completeObservedSpacesThrough(accepted.getObservedSpacesRevision());
    }

    private void acceptSpaceSnapshot(
            io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.SpaceSnapshot wire) {
        SpaceSnapshot snapshot = fromProtocolSpace(wire);
        SpaceSnapshot current = spaces.get(snapshot.spaceKey());
        if (current != null
                && current.incarnation().equals(snapshot.incarnation())
                && Long.compareUnsigned(current.spaceRevision(), snapshot.spaceRevision()) >= 0) {
            return;
        }
        spaces.put(snapshot.spaceKey(), snapshot);
        for (final SpaceListener listener : spaceListeners) {
            dispatch(new Runnable() {
                @Override
                public void run() {
                    listener.onSpaceUpdated(ControllerSession.this, snapshot);
                }
            });
        }
    }

    private void acceptSpaceClosed(
            io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.SpaceClosed wire) {
        final SpaceKey spaceKey = SpaceKey.of(wire.getSpaceKey());
        final SpaceIncarnation incarnation = new SpaceIncarnation(wire.getIncarnationId().toByteArray());
        SpaceSnapshot current = spaces.get(spaceKey);
        if (current == null || !current.incarnation().equals(incarnation)) {
            return;
        }
        spaces.remove(spaceKey);
        final long finalRevision = wire.getFinalSpaceRevision();
        for (final SpaceListener listener : spaceListeners) {
            dispatch(new Runnable() {
                @Override
                public void run() {
                    listener.onSpaceClosed(ControllerSession.this, spaceKey, incarnation, finalRevision);
                }
            });
        }
    }

    private void acceptFetch(ByteString requestId, FetchSpaceResult result) {
        CompletableFuture<SpaceSnapshot> future = pendingFetches.remove(requestId);
        if (future == null) {
            return;
        }
        switch (result.getResultCase()) {
            case SNAPSHOT:
                future.complete(fromProtocolSpace(result.getSnapshot()));
                break;
            case ABSENT:
                future.completeExceptionally(new CommandRejectedException(
                        "NOT_FOUND",
                        "space is not materialized: " + result.getAbsent().getSpaceKey()));
                break;
            case RESULT_NOT_SET:
            default:
                future.completeExceptionally(new ControllerException("FetchSpaceResult has no result"));
                break;
        }
    }

    private void acceptRejection(ByteString requestId, CommandRejected rejected) {
        RejectionHandler handler = rejectionHandlers.remove(requestId);
        CommandRejectedException failure = new CommandRejectedException(
                rejected.getCode().name(),
                rejected.getMessage());
        CompletableFuture<SpaceSnapshot> fetch = pendingFetches.remove(requestId);
        if (fetch != null) {
            fetch.completeExceptionally(failure);
        }
        if (handler == null) {
            failPermanently(new ControllerException("rejection has no matching request", failure));
            return;
        }
        handler.reject(failure);
    }

    private void beginResynchronization() {
        if (state == ControllerSessionState.STOPPING) {
            return;
        }
        reconciliationReceived = false;
        changeState(ControllerSessionState.RECONCILING);
        ByteString requestId = nextRequestId();
        SyncDesiredState sync = SyncDesiredState.newBuilder()
                .setSessionToken(sessionToken)
                .setDesiredState(buildDesiredStateSnapshot())
                .build();
        sendFrame(ClientFrame.newBuilder()
                .setRequestId(requestId)
                .setSyncDesiredState(sync)
                .build());
    }

    private void sendRegister(final ParticipantHandle handle) {
        ByteString requestId = nextRequestId();
        rejectionHandlers.put(requestId, new RejectionHandler() {
            @Override
            public void reject(CommandRejectedException failure) {
                handle.revoke(failure.getMessage());
                participants.remove(handle.participantId());
                desiredStateRevision++;
            }
        });
        RegisterParticipant register = RegisterParticipant.newBuilder()
                .setSessionToken(sessionToken)
                .setParticipant(toProtocolRegistration(handle))
                .build();
        sendFrame(ClientFrame.newBuilder()
                .setRequestId(requestId)
                .setRegisterParticipant(register)
                .build());
    }

    private void sendSpec(final ParticipantHandle handle, final long clientRevision) {
        ByteString requestId = nextRequestId();
        rejectionHandlers.put(requestId, new RejectionHandler() {
            @Override
            public void reject(CommandRejectedException failure) {
                if ("OWNERSHIP_LOST".equals(failure.code())) {
                    handle.revoke(failure.getMessage());
                    participants.remove(handle.participantId());
                    desiredStateRevision++;
                } else {
                    handle.specRejected(clientRevision, failure);
                }
            }
        });
        SetParticipantSpec setSpec = SetParticipantSpec.newBuilder()
                .setSessionToken(sessionToken)
                .setParticipantId(handle.participantId().value())
                .setOwnershipToken(handle.ownershipToken())
                .setClientSpecRevision(clientRevision)
                .setSpec(toProtocolSpec(handle.desiredSpec()))
                .build();
        sendFrame(ClientFrame.newBuilder()
                .setRequestId(requestId)
                .setSetParticipantSpec(setSpec)
                .build());
    }

    private void sendRelease(final ParticipantHandle handle) {
        ByteString requestId = nextRequestId();
        rejectionHandlers.put(requestId, new RejectionHandler() {
            @Override
            public void reject(CommandRejectedException failure) {
                CompletableFuture<Void> unregister = handle.unregisterFuture();
                if (unregister != null) {
                    unregister.completeExceptionally(failure);
                }
            }
        });
        ReleaseParticipant release = ReleaseParticipant.newBuilder()
                .setSessionToken(sessionToken)
                .setParticipantId(handle.participantId().value())
                .setRegistrationId(handle.registrationId())
                .setOwnershipToken(handle.ownershipToken())
                .build();
        sendFrame(ClientFrame.newBuilder()
                .setRequestId(requestId)
                .setReleaseParticipant(release)
                .build());
    }

    private void sendObservedSpaces(final CompletableFuture<Void> future) {
        ByteString requestId = nextRequestId();
        rejectionHandlers.put(requestId, new RejectionHandler() {
            @Override
            public void reject(CommandRejectedException failure) {
                future.completeExceptionally(failure);
            }
        });
        ReplaceObservedSpaces.Builder replace = ReplaceObservedSpaces.newBuilder()
                .setSessionToken(sessionToken)
                .setObservedSpacesRevision(observedSpacesRevision);
        for (SpaceKey spaceKey : explicitlyObservedSpaces) {
            replace.addSpaceKeys(spaceKey.value());
        }
        sendFrame(ClientFrame.newBuilder()
                .setRequestId(requestId)
                .setReplaceObservedSpaces(replace.build())
                .build());
    }

    private void sendFrame(ClientFrame frame) {
        if (transport == null) {
            return;
        }
        transport.send(frame).whenComplete((ignored, failure) -> {
            if (failure != null) {
                onTransportClosed(failure, true);
            }
        });
    }

    private DesiredStateSnapshot buildDesiredStateSnapshot() {
        DesiredStateSnapshot.Builder snapshot = DesiredStateSnapshot.newBuilder()
                .setDesiredStateRevision(desiredStateRevision);
        List<ParticipantHandle> ordered = new ArrayList<ParticipantHandle>(participants.values());
        Collections.sort(ordered, new Comparator<ParticipantHandle>() {
            @Override
            public int compare(ParticipantHandle first, ParticipantHandle second) {
                return first.participantId().value().compareTo(second.participantId().value());
            }
        });
        for (ParticipantHandle handle : ordered) {
            if (handle.isDesired()) {
                snapshot.addParticipants(toProtocolRegistration(handle));
            }
        }
        for (SpaceKey spaceKey : explicitlyObservedSpaces) {
            snapshot.addObservedSpaceKeys(spaceKey.value());
        }
        return snapshot.build();
    }

    private ParticipantRegistration toProtocolRegistration(ParticipantHandle handle) {
        return ParticipantRegistration.newBuilder()
                .setParticipantId(handle.participantId().value())
                .setRegistrationId(handle.registrationId())
                .setOwnershipToken(handle.ownershipToken())
                .setClientSpecRevision(handle.clientSpecRevision())
                .setSpec(toProtocolSpec(handle.desiredSpec()))
                .build();
    }

    private static io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.ParticipantSpec
            toProtocolSpec(ParticipantSpec spec) {
        return io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.ParticipantSpec
                .newBuilder()
                .setSpaceKey(spec.spaceKey().value())
                .setDisplayName(spec.displayName())
                .setServerMute(spec.serverMute())
                .setServerDeaf(spec.serverDeaf())
                .build();
    }

    private static SpaceSnapshot fromProtocolSpace(
            io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.SpaceSnapshot wire) {
        List<SpaceParticipant> participants = new ArrayList<SpaceParticipant>();
        for (io.github.theking90000.mumbleserverruntime.controller.internal.protocol.v1.SpaceParticipant
                participant : wire.getParticipantsList()) {
            participants.add(new SpaceParticipant(
                    ParticipantId.of(participant.getParticipantId()),
                    participant.getDisplayName(),
                    participant.getServerMute(),
                    participant.getServerDeaf(),
                    participant.getMumbleConnected()));
        }
        return new SpaceSnapshot(
                SpaceKey.of(wire.getSpaceKey()),
                new SpaceIncarnation(wire.getIncarnationId().toByteArray()),
                wire.getSpaceRevision(),
                participants,
                wire.getPublishedGeneration());
    }

    private void scheduleLeaseRenewal(final long leaseMillis) {
        cancelLeaseTask();
        long delayMillis = leaseMillis - leaseMillis / 3L;
        leaseTask = scheduler.schedule(new Runnable() {
            @Override
            public void run() {
                synchronized (monitor) {
                    if ((state != ControllerSessionState.ACTIVE
                                    && state != ControllerSessionState.RECONCILING)
                            || sessionToken.isEmpty()) {
                        return;
                    }
                    RenewLease renew = RenewLease.newBuilder()
                            .setSessionToken(sessionToken)
                            .setDesiredStateRevision(desiredStateRevision)
                            .build();
                    sendFrame(ClientFrame.newBuilder()
                            .setRequestId(nextRequestId())
                            .setRenewLease(renew)
                            .build());
                    scheduleLeaseRenewal(leaseMillis);
                }
            }
        }, delayMillis);
    }

    private void scheduleReconnect() {
        if (reconnectTask != null) {
            reconnectTask.cancel();
        }
        long exponent = 1L << Math.min(reconnectAttempt, 20);
        long base = Math.min(MAX_RECONNECT_MILLIS, INITIAL_RECONNECT_MILLIS * exponent);
        double factor = 1.0d - RECONNECT_JITTER + random.nextDouble() * RECONNECT_JITTER * 2.0d;
        long delayMillis = Math.max(1L, Math.round(base * factor));
        reconnectAttempt++;
        reconnectTask = scheduler.schedule(new Runnable() {
            @Override
            public void run() {
                synchronized (monitor) {
                    reconnectTask = null;
                    connectNow();
                }
            }
        }, delayMillis);
    }

    private void completeObservedSpacesThrough(long acceptedRevision) {
        Iterator<Map.Entry<Long, List<CompletableFuture<Void>>>> iterator =
                pendingObservedSpaces.entrySet().iterator();
        while (iterator.hasNext()) {
            Map.Entry<Long, List<CompletableFuture<Void>>> entry = iterator.next();
            if (Long.compareUnsigned(entry.getKey(), acceptedRevision) <= 0) {
                for (CompletableFuture<Void> future : entry.getValue()) {
                    future.complete(null);
                }
                iterator.remove();
            }
        }
    }

    private void removeClosedWithoutOwnership() {
        Iterator<Map.Entry<ParticipantId, ParticipantHandle>> iterator = participants.entrySet().iterator();
        while (iterator.hasNext()) {
            ParticipantHandle handle = iterator.next().getValue();
            if (handle.state() == ParticipantHandleState.CLOSED && handle.ownershipToken().isEmpty()) {
                handle.releaseAcknowledged();
                iterator.remove();
            }
        }
    }

    private void failPermanently(ControllerException failure) {
        if (state == ControllerSessionState.FAILED || state == ControllerSessionState.CLOSED) {
            return;
        }
        cancelScheduledTasks();
        if (transport != null) {
            transport.close();
            transport = null;
        }
        changeState(ControllerSessionState.FAILED);
        startFuture.completeExceptionally(failure);
        stopFuture.completeExceptionally(failure);
        for (CompletableFuture<SpaceSnapshot> future : pendingFetches.values()) {
            future.completeExceptionally(failure);
        }
        pendingFetches.clear();
        for (List<CompletableFuture<Void>> futures : pendingObservedSpaces.values()) {
            for (CompletableFuture<Void> future : futures) {
                future.completeExceptionally(failure);
            }
        }
        pendingObservedSpaces.clear();
        rejectionHandlers.clear();
        for (ParticipantHandle handle : participants.values()) {
            handle.beginUnregister();
            handle.releaseAcknowledged();
        }
        participants.clear();
        scheduler.close();
    }

    private void closeLocally() {
        cancelScheduledTasks();
        if (transport != null) {
            transport.close();
            transport = null;
        }
        SessionClosedException failure = new SessionClosedException("controller session closed");
        for (CompletableFuture<SpaceSnapshot> future : pendingFetches.values()) {
            future.completeExceptionally(failure);
        }
        pendingFetches.clear();
        for (List<CompletableFuture<Void>> futures : pendingObservedSpaces.values()) {
            for (CompletableFuture<Void> future : futures) {
                future.completeExceptionally(failure);
            }
        }
        pendingObservedSpaces.clear();
        for (ParticipantHandle handle : participants.values()) {
            handle.beginUnregister();
            handle.releaseAcknowledged();
        }
        participants.clear();
        changeState(ControllerSessionState.CLOSED);
        startFuture.completeExceptionally(failure);
        stopFuture.complete(null);
        rejectionHandlers.clear();
        scheduler.close();
    }

    private void changeState(ControllerSessionState next) {
        if (state == next) {
            return;
        }
        final ControllerSessionState previous = state;
        state = next;
        for (final ControllerSessionListener listener : sessionListeners) {
            dispatch(new Runnable() {
                @Override
                public void run() {
                    listener.onStateChanged(ControllerSession.this, previous, next);
                }
            });
        }
    }

    private void cancelScheduledTasks() {
        cancelLeaseTask();
        if (reconnectTask != null) {
            reconnectTask.cancel();
            reconnectTask = null;
        }
    }

    private void cancelLeaseTask() {
        if (leaseTask != null) {
            leaseTask.cancel();
            leaseTask = null;
        }
    }

    private void ensureNotTerminal() {
        if (state == ControllerSessionState.STOPPING
                || state == ControllerSessionState.CLOSED
                || state == ControllerSessionState.FAILED) {
            throw new SessionClosedException("controller session is " + state);
        }
    }

    private void ensureActive() {
        if (state != ControllerSessionState.ACTIVE) {
            throw new ControllerException("controller session is not active: " + state);
        }
    }

    private void requireCurrentHandle(ParticipantHandle handle) {
        Objects.requireNonNull(handle, "handle");
        if (participants.get(handle.participantId()) != handle) {
            throw new SessionClosedException("participant handle is no longer registered");
        }
        ensureNotTerminal();
    }

    private static ByteString randomIdentifier() {
        UUID value = UUID.randomUUID();
        ByteBuffer buffer = ByteBuffer.allocate(16);
        buffer.putLong(value.getMostSignificantBits());
        buffer.putLong(value.getLeastSignificantBits());
        return ByteString.copyFrom(buffer.array());
    }

    private static ByteString nextRequestId() {
        return randomIdentifier();
    }

    private static long durationMillis(Duration duration) {
        long secondsMillis = Math.multiplyExact(duration.getSeconds(), 1_000L);
        return Math.addExact(secondsMillis, duration.getNanos() / 1_000_000L);
    }

    private interface RejectionHandler {
        void reject(CommandRejectedException failure);
    }

    /** Builder for a single automatically reconnecting controller session. */
    public static final class Builder {
        private final ControllerId controllerId;
        private final URI endpoint;
        private Executor callbackExecutor = ForkJoinPool.commonPool();
        private TlsConfig tlsConfig;
        private ControllerTransport.Factory transportFactory;
        private SessionScheduler scheduler;
        private Random random;

        private Builder(ControllerId controllerId, URI endpoint) {
            this.controllerId = Objects.requireNonNull(controllerId, "controllerId");
            this.endpoint = validateEndpoint(endpoint);
        }

        public Builder callbackExecutor(Executor executor) {
            callbackExecutor = Objects.requireNonNull(executor, "executor");
            return this;
        }

        public Builder tls(TlsConfig config) {
            tlsConfig = Objects.requireNonNull(config, "config");
            return this;
        }

        public ControllerSession build() {
            if (tlsConfig == null && !"http".equalsIgnoreCase(endpoint.getScheme())) {
                throw new IllegalArgumentException("plaintext controller endpoints must use http");
            }
            if (tlsConfig != null && !"https".equalsIgnoreCase(endpoint.getScheme())) {
                throw new IllegalArgumentException("TLS controller endpoints must use https");
            }
            return new ControllerSession(this);
        }

        Builder transportFactory(ControllerTransport.Factory factory) {
            transportFactory = Objects.requireNonNull(factory, "factory");
            return this;
        }

        Builder scheduler(SessionScheduler value) {
            scheduler = Objects.requireNonNull(value, "scheduler");
            return this;
        }

        Builder random(Random value) {
            random = Objects.requireNonNull(value, "random");
            return this;
        }

        private static URI validateEndpoint(URI endpoint) {
            Objects.requireNonNull(endpoint, "endpoint");
            if (endpoint.getHost() == null || endpoint.getHost().trim().isEmpty()) {
                throw new IllegalArgumentException("controller endpoint must contain a host");
            }
            if (endpoint.getUserInfo() != null || endpoint.getQuery() != null || endpoint.getFragment() != null) {
                throw new IllegalArgumentException("controller endpoint must not contain credentials, query, or fragment");
            }
            return endpoint;
        }
    }
}
