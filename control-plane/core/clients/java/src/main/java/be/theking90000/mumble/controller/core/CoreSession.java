package be.theking90000.mumble.controller.core;

import be.theking90000.mumble.controller.internal.core.v1.ClientFrame;
import be.theking90000.mumble.controller.internal.core.v1.CloseSession;
import be.theking90000.mumble.controller.internal.core.v1.CommandErrorCode;
import be.theking90000.mumble.controller.internal.core.v1.CommandRejected;
import be.theking90000.mumble.controller.internal.core.v1.DesiredStateReconciled;
import be.theking90000.mumble.controller.internal.core.v1.DesiredStateSnapshot;
import be.theking90000.mumble.controller.internal.core.v1.OpenSession;
import be.theking90000.mumble.controller.internal.core.v1.ParticipantOwnershipGranted;
import be.theking90000.mumble.controller.internal.core.v1.ParticipantOwnershipRevoked;
import be.theking90000.mumble.controller.internal.core.v1.ParticipantRegistration;
import be.theking90000.mumble.controller.internal.core.v1.ParticipantSpecAccepted;
import be.theking90000.mumble.controller.internal.core.v1.ParticipantStatusChanged;
import be.theking90000.mumble.controller.internal.core.v1.ProfileCommand;
import be.theking90000.mumble.controller.internal.core.v1.ProfileEvent;
import be.theking90000.mumble.controller.internal.core.v1.ProfileRef;
import be.theking90000.mumble.controller.internal.core.v1.RegisterParticipant;
import be.theking90000.mumble.controller.internal.core.v1.ReleaseParticipant;
import be.theking90000.mumble.controller.internal.core.v1.RenewLease;
import be.theking90000.mumble.controller.internal.core.v1.ServerFrame;
import be.theking90000.mumble.controller.internal.core.v1.SessionReady;
import be.theking90000.mumble.controller.internal.core.v1.SetParticipantSpec;
import be.theking90000.mumble.controller.internal.core.v1.SyncDesiredState;
import com.google.protobuf.ByteString;
import com.google.protobuf.Duration;
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
import java.util.TreeMap;
import java.util.UUID;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.Executor;
import java.util.concurrent.ForkJoinPool;
import java.util.function.Supplier;

/**
 * Profile-neutral owner of Controller sessions, leases, ownership, and reliable commands.
 *
 * <p>The Core treats all profile state, participant specifications, statuses, commands, and events
 * as encoded Protobuf payloads. A typed implementation owns their interpretation. Session
 * reconnection always resends one complete desired-state snapshot.</p>
 */
public final class CoreSession {
    private final Object monitor = new Object();
    private final CoreSessionLifecycle lifecycle = new CoreSessionLifecycle();
    private final ControllerId controllerId;
    private final ProfileReference profile;
    private final ByteString controllerInstanceId;
    private final CoreTransport.Factory transportFactory;
    private final SessionScheduler scheduler;
    private final SerializingExecutor callbackExecutor;
    private final Random random;
    private final Map<ParticipantId, CoreParticipantHandle> participants =
            new LinkedHashMap<ParticipantId, CoreParticipantHandle>();
    private final CopyOnWriteArrayList<CoreSessionListener> sessionListeners =
            new CopyOnWriteArrayList<CoreSessionListener>();
    private final CopyOnWriteArrayList<ProfileEventListener> profileEventListeners =
            new CopyOnWriteArrayList<ProfileEventListener>();
    private final NavigableMap<Long, List<CompletableFuture<Void>>> pendingProfileStates =
            new TreeMap<Long, List<CompletableFuture<Void>>>();
    private final Map<ByteString, CompletableFuture<ProfilePayload>> pendingProfileCommands =
            new HashMap<ByteString, CompletableFuture<ProfilePayload>>();
    private final ReliableRequestTracker reliableRequests =
            new ReliableRequestTracker(new Supplier<ByteString>() {
                @Override
                public ByteString get() {
                    return randomIdentifier();
                }
            });
    private final CompletableFuture<Void> startFuture = new CompletableFuture<Void>();
    private final CompletableFuture<Void> stopFuture = new CompletableFuture<Void>();

    private ProfilePayload profileState;
    private CoreTransport transport;
    private ByteString resumeToken = ByteString.EMPTY;
    private ByteString sessionToken = ByteString.EMPTY;
    private ByteString leaseRequestId = ByteString.EMPTY;
    private ByteString syncRequestId = ByteString.EMPTY;
    private long desiredStateRevision;
    private long reconciledStateRevision;
    private boolean sessionReady;
    private boolean reconciliationReceived;
    private SessionScheduler.Cancellable reconnectTask;
    private SessionScheduler.Cancellable leaseTask;

    private CoreSession(Builder builder) {
        controllerId = builder.controllerId;
        profile = builder.profile;
        profileState = builder.initialProfileState;
        controllerInstanceId = randomIdentifier();
        callbackExecutor = new SerializingExecutor(builder.callbackExecutor);
        scheduler = builder.scheduler == null ? new SessionScheduler.Default() : builder.scheduler;
        random = builder.random == null ? new Random() : builder.random;
        if (builder.transportFactory == null) {
            final URI endpoint = builder.endpoint;
            final TlsConfig tlsConfig = builder.tlsConfig;
            transportFactory = new CoreTransport.Factory() {
                @Override
                public CoreTransport create() {
                    return new GrpcCoreTransport(endpoint, tlsConfig);
                }
            };
        } else {
            transportFactory = builder.transportFactory;
        }
    }

    /**
     * Creates a builder for a logical controller and one exact profile schema.
     *
     * @param controllerId stable declarative controller identity
     * @param endpoint Controller service URI
     * @param profile exact profile schema to negotiate
     * @return a new Core session builder
     */
    public static Builder builder(
            ControllerId controllerId, URI endpoint, ProfileReference profile) {
        return new Builder(controllerId, endpoint, profile);
    }

    /** @return declarative controller identity */
    public ControllerId controllerId() {
        return controllerId;
    }

    /** @return exact profile schema negotiated by this session */
    public ProfileReference profile() {
        return profile;
    }

    /** @return current observable lifecycle state */
    public ControllerSessionState state() {
        synchronized (monitor) {
            return lifecycle.state();
        }
    }

    /**
     * Starts the transport and initial desired-state reconciliation.
     *
     * @return stable future completed when the session first becomes active
     */
    public CompletableFuture<Void> start() {
        synchronized (monitor) {
            if (lifecycle.state() != ControllerSessionState.NEW) {
                return startFuture;
            }
            changeState(ControllerSessionState.CONNECTING);
            connectNow();
            return startFuture;
        }
    }

    /**
     * Declares one participant and its complete encoded profile specification.
     *
     * @param participantId stable logical identity
     * @param spec complete encoded profile specification
     * @return new ownership handle
     */
    public CoreParticipantHandle registerParticipant(
            ParticipantId participantId, ProfilePayload spec) {
        Objects.requireNonNull(participantId, "participantId");
        Objects.requireNonNull(spec, "spec");
        synchronized (monitor) {
            ensureNotTerminal();
            CoreParticipantHandle existing = participants.get(participantId);
            if (existing != null) {
                if (existing.isDesired()
                        || existing.unregisterFuture() == null
                        || !existing.unregisterFuture().isDone()) {
                    throw new IllegalStateException(
                            "participant registration or release is already in progress: "
                                    + participantId);
                }
                participants.remove(participantId);
            }
            CoreParticipantHandle handle = new CoreParticipantHandle(
                    this, participantId, randomIdentifier(), spec);
            participants.put(participantId, handle);
            desiredStateRevision++;
            if (lifecycle.state() == ControllerSessionState.ACTIVE) {
                sendRegister(handle);
            }
            return handle;
        }
    }

    /**
     * Looks up a current desired participant.
     *
     * @param participantId identity to find
     * @return desired handle, or empty after release or revocation
     */
    public Optional<CoreParticipantHandle> participant(ParticipantId participantId) {
        Objects.requireNonNull(participantId, "participantId");
        synchronized (monitor) {
            CoreParticipantHandle handle = participants.get(participantId);
            return handle != null && handle.isDesired()
                    ? Optional.of(handle)
                    : Optional.<CoreParticipantHandle>empty();
        }
    }

    /** @return immutable point-in-time map of desired participants */
    public Map<ParticipantId, CoreParticipantHandle> participants() {
        synchronized (monitor) {
            Map<ParticipantId, CoreParticipantHandle> snapshot =
                    new LinkedHashMap<ParticipantId, CoreParticipantHandle>();
            for (Map.Entry<ParticipantId, CoreParticipantHandle> entry : participants.entrySet()) {
                if (entry.getValue().isDesired()) {
                    snapshot.put(entry.getKey(), entry.getValue());
                }
            }
            return Collections.unmodifiableMap(snapshot);
        }
    }

    /** @return latest complete encoded desired profile state */
    public ProfilePayload profileState() {
        synchronized (monitor) {
            return profileState;
        }
    }

    /**
     * Replaces the complete encoded desired profile state.
     *
     * @param state complete encoded profile state
     * @return future completed after reconciliation covers this revision
     */
    public CompletableFuture<Void> setProfileState(ProfilePayload state) {
        Objects.requireNonNull(state, "state");
        synchronized (monitor) {
            ensureNotTerminal();
            profileState = state;
            desiredStateRevision++;
            CompletableFuture<Void> future = new CompletableFuture<Void>();
            List<CompletableFuture<Void>> pending = pendingProfileStates.get(desiredStateRevision);
            if (pending == null) {
                pending = new ArrayList<CompletableFuture<Void>>();
                pendingProfileStates.put(desiredStateRevision, pending);
            }
            pending.add(future);
            if (lifecycle.state() == ControllerSessionState.ACTIVE) {
                beginResynchronization();
            }
            return future;
        }
    }

    /**
     * Sends one reliable profile command on a ready stream.
     *
     * <p>The future completes with the first profile event carrying the same request identifier,
     * or exceptionally if the command is rejected or its stream closes.</p>
     *
     * @param command complete encoded profile command
     * @return correlated profile response future
     */
    public CompletableFuture<ProfilePayload> sendProfileCommand(ProfilePayload command) {
        return sendProfileCommandInternal(command, null);
    }

    /**
     * Sends a reliable profile command and records the complete state it produces for replay.
     *
     * <p>The resulting state is included in every later reconnect snapshot without forcing a
     * redundant desired-state synchronization on the current stream. The correlated profile event
     * remains the acceptance barrier for the command.</p>
     *
     * @param command complete encoded profile command
     * @param resultingState complete encoded state after the command is accepted
     * @return correlated profile response future
     */
    public CompletableFuture<ProfilePayload> sendProfileCommand(
            ProfilePayload command, ProfilePayload resultingState) {
        Objects.requireNonNull(resultingState, "resultingState");
        return sendProfileCommandInternal(command, resultingState);
    }

    private CompletableFuture<ProfilePayload> sendProfileCommandInternal(
            ProfilePayload command, ProfilePayload resultingState) {
        Objects.requireNonNull(command, "command");
        synchronized (monitor) {
            ensureProfileCommandAvailable();
            if (resultingState != null) {
                profileState = resultingState;
                desiredStateRevision++;
            }
            final CompletableFuture<ProfilePayload> future =
                    new CompletableFuture<ProfilePayload>();
            ByteString requestId = reliableRequests.track(
                    new ReliableRequestTracker.RejectionHandler() {
                @Override
                public void reject(CommandRejectedException failure) {
                    future.completeExceptionally(failure);
                }
            });
            pendingProfileCommands.put(requestId, future);
            sendFrame(ClientFrame.newBuilder()
                    .setRequestId(requestId)
                    .setProfileCommand(ProfileCommand.newBuilder()
                            .setSessionToken(sessionToken)
                            .setPayload(toWirePayload(command))
                            .build())
                    .build());
            return future;
        }
    }

    /**
     * Stops renewal, closes the remote session when possible, and releases local resources.
     *
     * @return stable future completed when the session is locally closed
     */
    public CompletableFuture<Void> stop() {
        synchronized (monitor) {
            if (lifecycle.state() == ControllerSessionState.CLOSED) {
                return stopFuture;
            }
            if (lifecycle.state() == ControllerSessionState.NEW
                    || lifecycle.state() == ControllerSessionState.FAILED) {
                closeLocally();
                return stopFuture;
            }
            if (lifecycle.state() == ControllerSessionState.STOPPING) {
                return stopFuture;
            }
            changeState(ControllerSessionState.STOPPING);
            cancelScheduledTasks();
            if (transport == null || sessionToken.isEmpty()) {
                closeLocally();
                return stopFuture;
            }
            ByteString requestId = reliableRequests.track(
                    new ReliableRequestTracker.RejectionHandler() {
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

    /**
     * Adds a listener for subsequent session transitions.
     *
     * @param listener listener to add
     */
    public void addSessionListener(CoreSessionListener listener) {
        sessionListeners.add(Objects.requireNonNull(listener, "listener"));
    }

    /**
     * Removes a session listener; a missing listener is ignored.
     *
     * @param listener listener to remove
     */
    public void removeSessionListener(CoreSessionListener listener) {
        sessionListeners.remove(listener);
    }

    /**
     * Adds a listener for subsequent uncorrelated profile events.
     *
     * @param listener listener to add
     */
    public void addProfileEventListener(ProfileEventListener listener) {
        profileEventListeners.add(Objects.requireNonNull(listener, "listener"));
    }

    /**
     * Removes a profile event listener; a missing listener is ignored.
     *
     * @param listener listener to remove
     */
    public void removeProfileEventListener(ProfileEventListener listener) {
        profileEventListeners.remove(listener);
    }

    /**
     * Fails this session after the negotiated profile rejects a decoded payload.
     *
     * <p>Typed profile facades call this fail-closed boundary when an envelope is structurally
     * valid Core protocol but its negotiated profile payload is missing, malformed, or unexpected.
     * No reconnection is attempted for a deterministic schema violation.</p>
     *
     * @param failure profile decoding or semantic protocol failure
     */
    public void reportProfileProtocolFailure(ControllerException failure) {
        Objects.requireNonNull(failure, "failure");
        synchronized (monitor) {
            failPermanently(failure);
        }
    }

    Object monitor() {
        return monitor;
    }

    void dispatch(Runnable callback) {
        callbackExecutor.execute(callback);
    }

    CompletableFuture<AcceptedRevision> setParticipantSpec(
            CoreParticipantHandle handle, ProfilePayload spec) {
        synchronized (monitor) {
            requireCurrentHandle(handle);
            CompletableFuture<AcceptedRevision> future = handle.replaceDesiredSpec(spec);
            desiredStateRevision++;
            if (lifecycle.state() == ControllerSessionState.ACTIVE
                    && handle.state() == ParticipantHandleState.OWNED) {
                sendSpec(handle, handle.clientSpecRevision());
            }
            return future;
        }
    }

    CompletableFuture<Void> unregisterParticipant(CoreParticipantHandle handle) {
        synchronized (monitor) {
            requireCurrentHandle(handle);
            CompletableFuture<Void> future = handle.beginUnregister();
            desiredStateRevision++;
            if (handle.ownershipToken().isEmpty()) {
                if (lifecycle.state() == ControllerSessionState.NEW) {
                    handle.releaseAcknowledged();
                    participants.remove(handle.participantId());
                }
                return future;
            }
            if (lifecycle.state() == ControllerSessionState.ACTIVE) {
                sendRelease(handle);
            }
            return future;
        }
    }

    private void connectNow() {
        if (lifecycle.state() == ControllerSessionState.CLOSED
                || lifecycle.state() == ControllerSessionState.STOPPING
                || lifecycle.state() == ControllerSessionState.FAILED) {
            return;
        }
        try {
            final CoreTransport created = transportFactory.create();
            transport = created;
            created.connect(new CoreTransport.Listener() {
                @Override
                public void onConnected() {
                    CoreSession.this.onTransportConnected(created);
                }

                @Override
                public void onFrame(ServerFrame frame) {
                    CoreSession.this.onServerFrame(created, frame);
                }

                @Override
                public void onClosed(Throwable failure, boolean retryable) {
                    CoreSession.this.onTransportClosed(created, failure, retryable);
                }
            });
        } catch (RuntimeException failure) {
            handleTransportFailure(failure, true);
        }
    }

    private void onTransportConnected(CoreTransport source) {
        synchronized (monitor) {
            if (source != transport) {
                source.close();
                return;
            }
            if (lifecycle.state() == ControllerSessionState.STOPPING
                    || lifecycle.state() == ControllerSessionState.CLOSED) {
                source.close();
                return;
            }
            sessionReady = false;
            reconciliationReceived = false;
            sessionToken = ByteString.EMPTY;
            changeState(ControllerSessionState.RECONCILING);
            ByteString requestId = reliableRequests.track(
                    new ReliableRequestTracker.RejectionHandler() {
                @Override
                public void reject(CommandRejectedException failure) {
                    failPermanently(failure);
                }
            });
            sendFrame(ClientFrame.newBuilder()
                    .setRequestId(requestId)
                    .setOpenSession(OpenSession.newBuilder()
                            .setControllerId(controllerId.value())
                            .setControllerInstanceId(controllerInstanceId)
                            .setResumeToken(resumeToken)
                            .setDesiredState(buildDesiredStateSnapshot())
                            .setProfile(toWireProfile(profile))
                            .build())
                    .build());
        }
    }

    private void onServerFrame(CoreTransport source, ServerFrame frame) {
        Objects.requireNonNull(frame, "frame");
        synchronized (monitor) {
            if (source != transport
                    || lifecycle.state() == ControllerSessionState.CLOSED
                    || lifecycle.state() == ControllerSessionState.FAILED) {
                return;
            }
            switch (frame.getPayloadCase()) {
                case SESSION_READY:
                    reliableRequests.complete(frame.getRequestId());
                    acceptSession(frame.getSessionReady());
                    break;
                case DESIRED_STATE_RECONCILED:
                    reliableRequests.complete(frame.getRequestId());
                    acceptReconciliation(frame.getDesiredStateReconciled());
                    break;
                case PARTICIPANT_OWNERSHIP_GRANTED:
                    reliableRequests.complete(frame.getRequestId());
                    acceptOwnership(frame.getParticipantOwnershipGranted());
                    break;
                case PARTICIPANT_OWNERSHIP_REVOKED:
                    reliableRequests.complete(frame.getRequestId());
                    acceptRevocation(frame.getParticipantOwnershipRevoked());
                    break;
                case PARTICIPANT_SPEC_ACCEPTED:
                    reliableRequests.complete(frame.getRequestId());
                    acceptSpec(frame.getParticipantSpecAccepted());
                    break;
                case PARTICIPANT_STATUS_CHANGED:
                    acceptStatus(frame.getParticipantStatusChanged());
                    break;
                case PROFILE_EVENT:
                    acceptProfileEvent(frame.getRequestId(), frame.getProfileEvent());
                    break;
                case RESYNC_REQUIRED:
                    beginResynchronization();
                    break;
                case COMMAND_REJECTED:
                    acceptRejection(frame.getRequestId(), frame.getCommandRejected());
                    break;
                case SESSION_CLOSING:
                    reliableRequests.complete(frame.getRequestId());
                    closeLocally();
                    break;
                case PAYLOAD_NOT_SET:
                default:
                    failPermanently(new ControllerException(
                            "server frame has no supported payload"));
                    break;
            }
        }
    }

    private void onTransportClosed(CoreTransport source, Throwable failure, boolean retryable) {
        synchronized (monitor) {
            if (source == transport) {
                handleTransportFailure(failure, retryable);
            }
        }
    }

    private void handleTransportFailure(Throwable failure, boolean retryable) {
        if (lifecycle.state() == ControllerSessionState.STOPPING) {
            closeLocally();
            return;
        }
        if (lifecycle.state() == ControllerSessionState.CLOSED
                || lifecycle.state() == ControllerSessionState.FAILED) {
            return;
        }
        if (!retryable) {
            failPermanently(new ControllerException(
                    "permanent controller transport failure", failure));
            return;
        }
        cancelLeaseTask();
        if (transport != null) {
            transport.close();
            transport = null;
        }
        ControllerException disconnected = new ControllerException(
                "controller stream closed before the response arrived", failure);
        failProfileCommands(disconnected);
        clearRejectionHandlers();
        for (CoreParticipantHandle handle : participants.values()) {
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
        if (!ready.hasProfile() || !matches(profile, ready.getProfile())) {
            failPermanently(new ControllerException(
                    "SessionReady selected an unexpected profile"));
            return;
        }
        long leaseMillis;
        try {
            leaseMillis = durationMillis(ready.getLeaseDuration());
        } catch (ArithmeticException failure) {
            failPermanently(new ControllerException(
                    "lease duration overflows milliseconds", failure));
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
            failPermanently(new ControllerException(
                    "server reconciled an unknown desired-state revision"));
            return;
        }
        removeClosedWithoutOwnership();
        completeProfileStatesThrough(reconciledStateRevision);
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
        lifecycle.resetReconnectBackoff();
        changeState(ControllerSessionState.ACTIVE);
        startFuture.complete(null);
        for (CoreParticipantHandle handle :
                new ArrayList<CoreParticipantHandle>(participants.values())) {
            if (handle.state() == ParticipantHandleState.CLOSED
                    && !handle.ownershipToken().isEmpty()) {
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
        CoreParticipantHandle handle = participants.get(participantId);
        if (handle == null || !handle.registrationId().equals(granted.getRegistrationId())) {
            return;
        }
        if (granted.getOwnershipToken().isEmpty()) {
            failPermanently(new ControllerException(
                    "ownership grant contains an empty token"));
            return;
        }
        if (granted.getConnectionCredential().isEmpty()) {
            failPermanently(new ControllerException(
                    "ownership grant contains an empty connection credential"));
            return;
        }
        handle.ownershipGranted(
                granted.getOwnershipToken(),
                granted.getConnectionCredential(),
                granted.getClientSpecRevision(),
                granted.getAcceptedSpecRevision(),
                granted.getAppliedSpecRevision(),
                granted.getPublishedGeneration());
        if (handle.state() == ParticipantHandleState.CLOSED) {
            sendRelease(handle);
        } else if (Long.compareUnsigned(
                handle.clientSpecRevision(), handle.acceptedClientSpecRevision()) > 0) {
            sendSpec(handle, handle.clientSpecRevision());
        }
    }

    private void acceptRevocation(ParticipantOwnershipRevoked revoked) {
        ParticipantId participantId = ParticipantId.of(revoked.getParticipantId());
        CoreParticipantHandle handle = participants.get(participantId);
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
        CoreParticipantHandle handle = participants.get(
                ParticipantId.of(accepted.getParticipantId()));
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
        CoreParticipantHandle handle = participants.get(
                ParticipantId.of(changed.getParticipantId()));
        if (handle == null || !handle.isDesired()) {
            return;
        }
        be.theking90000.mumble.controller.internal.core.v1.ParticipantStatus status =
                changed.getStatus();
        if (!status.hasProfileStatus()) {
            failPermanently(new ControllerException(
                    "participant status profile payload is missing"));
            return;
        }
        handle.updateStatus(new CoreParticipantStatus(
                status.getConnected(),
                status.getAcceptedSpecRevision(),
                status.getAppliedSpecRevision(),
                status.getPublishedGeneration(),
                status.getApplicationError(),
                fromWirePayload(status.getProfileStatus())));
    }

    private void acceptProfileEvent(ByteString requestId, ProfileEvent event) {
        if (!event.hasPayload()) {
            failPermanently(new ControllerException("profile event payload is missing"));
            return;
        }
        ProfilePayload payload = fromWirePayload(event.getPayload());
        CompletableFuture<ProfilePayload> pending = pendingProfileCommands.remove(requestId);
        if (pending != null) {
            reliableRequests.complete(requestId);
            pending.complete(payload);
            return;
        }
        for (final ProfileEventListener listener : profileEventListeners) {
            final ProfilePayload dispatched = payload;
            dispatch(new Runnable() {
                @Override
                public void run() {
                    listener.onProfileEvent(CoreSession.this, dispatched);
                }
            });
        }
    }

    private void acceptRejection(ByteString requestId, CommandRejected rejected) {
        ReliableRequestTracker.RejectionHandler handler = reliableRequests.take(requestId);
        CommandRejectedException failure = new CommandRejectedException(
                rejected.getCode().name(), rejected.getMessage());
        pendingProfileCommands.remove(requestId);
        if (handler == null) {
            failPermanently(new ControllerException(
                    "rejection has no matching request", failure));
            return;
        }
        handler.reject(failure);
    }

    private ByteString trackSessionScopedRequest(
            ByteString previousRequestId, final String context) {
        return reliableRequests.replace(
                previousRequestId,
                new ReliableRequestTracker.RejectionHandler() {
            @Override
            public void reject(CommandRejectedException failure) {
                if (CommandErrorCode.SESSION_EXPIRED_ERROR.name().equals(failure.code())) {
                    handleTransportFailure(failure, true);
                    return;
                }
                failPermanently(new ControllerException(context, failure));
            }
                });
    }

    private void beginResynchronization() {
        if (lifecycle.state() == ControllerSessionState.STOPPING) {
            return;
        }
        reconciliationReceived = false;
        changeState(ControllerSessionState.RECONCILING);
        ByteString requestId = trackSessionScopedRequest(
                syncRequestId, "desired-state synchronization was rejected");
        syncRequestId = requestId;
        sendFrame(ClientFrame.newBuilder()
                .setRequestId(requestId)
                .setSyncDesiredState(SyncDesiredState.newBuilder()
                        .setSessionToken(sessionToken)
                        .setDesiredState(buildDesiredStateSnapshot())
                        .build())
                .build());
    }

    private void sendRegister(final CoreParticipantHandle handle) {
        ByteString requestId = reliableRequests.track(
                new ReliableRequestTracker.RejectionHandler() {
            @Override
            public void reject(CommandRejectedException failure) {
                handle.revoke(failure.getMessage());
                participants.remove(handle.participantId());
                desiredStateRevision++;
            }
        });
        sendFrame(ClientFrame.newBuilder()
                .setRequestId(requestId)
                .setRegisterParticipant(RegisterParticipant.newBuilder()
                        .setSessionToken(sessionToken)
                        .setParticipant(toProtocolRegistration(handle))
                        .build())
                .build());
    }

    private void sendSpec(final CoreParticipantHandle handle, final long clientRevision) {
        ByteString requestId = reliableRequests.track(
                new ReliableRequestTracker.RejectionHandler() {
            @Override
            public void reject(CommandRejectedException failure) {
                if (CommandErrorCode.OWNERSHIP_LOST.name().equals(failure.code())) {
                    handle.revoke(failure.getMessage());
                    participants.remove(handle.participantId());
                    desiredStateRevision++;
                } else {
                    handle.specRejected(clientRevision, failure);
                }
            }
        });
        sendFrame(ClientFrame.newBuilder()
                .setRequestId(requestId)
                .setSetParticipantSpec(SetParticipantSpec.newBuilder()
                        .setSessionToken(sessionToken)
                        .setParticipantId(handle.participantId().value())
                        .setOwnershipToken(handle.ownershipToken())
                        .setClientSpecRevision(clientRevision)
                        .setProfileSpec(toWirePayload(handle.desiredSpec()))
                        .build())
                .build());
    }

    private void sendRelease(final CoreParticipantHandle handle) {
        ByteString requestId = reliableRequests.track(
                new ReliableRequestTracker.RejectionHandler() {
            @Override
            public void reject(CommandRejectedException failure) {
                CompletableFuture<Void> unregister = handle.unregisterFuture();
                if (unregister != null) {
                    unregister.completeExceptionally(failure);
                }
            }
        });
        sendFrame(ClientFrame.newBuilder()
                .setRequestId(requestId)
                .setReleaseParticipant(ReleaseParticipant.newBuilder()
                        .setSessionToken(sessionToken)
                        .setParticipantId(handle.participantId().value())
                        .setRegistrationId(handle.registrationId())
                        .setOwnershipToken(handle.ownershipToken())
                        .build())
                .build());
    }

    private void sendFrame(ClientFrame frame) {
        final CoreTransport current = transport;
        if (current == null) {
            return;
        }
        current.send(frame).whenComplete((ignored, failure) -> {
            if (failure != null) {
                onTransportClosed(current, failure, true);
            }
        });
    }

    private DesiredStateSnapshot buildDesiredStateSnapshot() {
        DesiredStateSnapshot.Builder snapshot = DesiredStateSnapshot.newBuilder()
                .setDesiredStateRevision(desiredStateRevision)
                .setProfileState(toWirePayload(profileState));
        List<CoreParticipantHandle> ordered =
                new ArrayList<CoreParticipantHandle>(participants.values());
        Collections.sort(ordered, new Comparator<CoreParticipantHandle>() {
            @Override
            public int compare(CoreParticipantHandle first, CoreParticipantHandle second) {
                return first.participantId().value().compareTo(second.participantId().value());
            }
        });
        for (CoreParticipantHandle handle : ordered) {
            if (handle.isDesired()) {
                snapshot.addParticipants(toProtocolRegistration(handle));
            }
        }
        return snapshot.build();
    }

    private ParticipantRegistration toProtocolRegistration(CoreParticipantHandle handle) {
        return ParticipantRegistration.newBuilder()
                .setParticipantId(handle.participantId().value())
                .setRegistrationId(handle.registrationId())
                .setOwnershipToken(handle.ownershipToken())
                .setClientSpecRevision(handle.clientSpecRevision())
                .setProfileSpec(toWirePayload(handle.desiredSpec()))
                .build();
    }

    private void scheduleLeaseRenewal(final long leaseMillis) {
        cancelLeaseTask();
        final long delayMillis = leaseMillis - leaseMillis / 3L;
        leaseTask = scheduler.schedule(new Runnable() {
            @Override
            public void run() {
                synchronized (monitor) {
                    if ((lifecycle.state() != ControllerSessionState.ACTIVE
                                    && lifecycle.state() != ControllerSessionState.RECONCILING)
                            || sessionToken.isEmpty()) {
                        return;
                    }
                    ByteString requestId = trackSessionScopedRequest(
                            leaseRequestId, "lease renewal was rejected");
                    leaseRequestId = requestId;
                    sendFrame(ClientFrame.newBuilder()
                            .setRequestId(requestId)
                            .setRenewLease(RenewLease.newBuilder()
                                    .setSessionToken(sessionToken)
                                    .setDesiredStateRevision(desiredStateRevision)
                                    .build())
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
        long delayMillis = lifecycle.nextReconnectDelayMillis(random.nextDouble());
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

    private void completeProfileStatesThrough(long reconciledRevision) {
        Iterator<Map.Entry<Long, List<CompletableFuture<Void>>>> iterator =
                pendingProfileStates.entrySet().iterator();
        while (iterator.hasNext()) {
            Map.Entry<Long, List<CompletableFuture<Void>>> entry = iterator.next();
            if (Long.compareUnsigned(entry.getKey(), reconciledRevision) <= 0) {
                for (CompletableFuture<Void> future : entry.getValue()) {
                    future.complete(null);
                }
                iterator.remove();
            }
        }
    }

    private void removeClosedWithoutOwnership() {
        Iterator<Map.Entry<ParticipantId, CoreParticipantHandle>> iterator =
                participants.entrySet().iterator();
        while (iterator.hasNext()) {
            CoreParticipantHandle handle = iterator.next().getValue();
            if (handle.state() == ParticipantHandleState.CLOSED
                    && handle.ownershipToken().isEmpty()) {
                handle.releaseAcknowledged();
                iterator.remove();
            }
        }
    }

    private void failPermanently(ControllerException failure) {
        if (lifecycle.state() == ControllerSessionState.FAILED
                || lifecycle.state() == ControllerSessionState.CLOSED) {
            return;
        }
        if (lifecycle.state() == ControllerSessionState.STOPPING) {
            closeLocally();
            return;
        }
        cancelScheduledTasks();
        if (transport != null) {
            transport.close();
            transport = null;
        }
        changeState(ControllerSessionState.FAILED);
        startFuture.completeExceptionally(failure);
        failPendingProfileStates(failure);
        failProfileCommands(failure);
        clearRejectionHandlers();
        for (CoreParticipantHandle handle : participants.values()) {
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
        SessionClosedException failure = new SessionClosedException(
                "controller session closed");
        failPendingProfileStates(failure);
        failProfileCommands(failure);
        for (CoreParticipantHandle handle : participants.values()) {
            handle.beginUnregister();
            handle.releaseAcknowledged();
        }
        participants.clear();
        changeState(ControllerSessionState.CLOSED);
        startFuture.completeExceptionally(failure);
        stopFuture.complete(null);
        clearRejectionHandlers();
        scheduler.close();
    }

    private void failPendingProfileStates(Throwable failure) {
        for (List<CompletableFuture<Void>> futures : pendingProfileStates.values()) {
            for (CompletableFuture<Void> future : futures) {
                future.completeExceptionally(failure);
            }
        }
        pendingProfileStates.clear();
    }

    private void failProfileCommands(Throwable failure) {
        for (CompletableFuture<ProfilePayload> future : pendingProfileCommands.values()) {
            future.completeExceptionally(failure);
        }
        pendingProfileCommands.clear();
    }

    private void clearRejectionHandlers() {
        reliableRequests.clear();
        leaseRequestId = ByteString.EMPTY;
        syncRequestId = ByteString.EMPTY;
    }

    private void changeState(ControllerSessionState next) {
        if (lifecycle.state() == next) {
            return;
        }
        final ControllerSessionState previous = lifecycle.transitionTo(next);
        for (final CoreSessionListener listener : sessionListeners) {
            dispatch(new Runnable() {
                @Override
                public void run() {
                    listener.onStateChanged(CoreSession.this, previous, next);
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
        if (lifecycle.state() == ControllerSessionState.STOPPING
                || lifecycle.state() == ControllerSessionState.CLOSED
                || lifecycle.state() == ControllerSessionState.FAILED) {
            throw new SessionClosedException("controller session is " + lifecycle.state());
        }
    }

    private void ensureProfileCommandAvailable() {
        if ((lifecycle.state() != ControllerSessionState.ACTIVE
                        && lifecycle.state() != ControllerSessionState.RECONCILING)
                || !sessionReady
                || sessionToken.isEmpty()) {
            throw new ControllerException(
                    "controller profile commands are not available: " + lifecycle.state());
        }
    }

    private void requireCurrentHandle(CoreParticipantHandle handle) {
        Objects.requireNonNull(handle, "handle");
        if (participants.get(handle.participantId()) != handle) {
            throw new SessionClosedException("participant handle is no longer registered");
        }
        ensureNotTerminal();
    }

    private static ProfileRef toWireProfile(ProfileReference value) {
        return ProfileRef.newBuilder()
                .setProfileId(value.profileId())
                .setSchemaVersion(value.schemaVersion())
                .setDescriptorDigest(value.descriptorDigest())
                .build();
    }

    private static boolean matches(ProfileReference expected, ProfileRef actual) {
        return expected.profileId().equals(actual.getProfileId())
                && expected.schemaVersion() == actual.getSchemaVersion()
                && expected.descriptorDigest().equals(actual.getDescriptorDigest());
    }

    private static be.theking90000.mumble.controller.internal.core.v1.ProfilePayload
            toWirePayload(ProfilePayload value) {
        return be.theking90000.mumble.controller.internal.core.v1.ProfilePayload.newBuilder()
                .setProtobuf(value.toByteString())
                .build();
    }

    private static ProfilePayload fromWirePayload(
            be.theking90000.mumble.controller.internal.core.v1.ProfilePayload value) {
        return ProfilePayload.fromByteString(value.getProtobuf());
    }

    private static ByteString randomIdentifier() {
        UUID value = UUID.randomUUID();
        ByteBuffer buffer = ByteBuffer.allocate(16);
        buffer.putLong(value.getMostSignificantBits());
        buffer.putLong(value.getLeastSignificantBits());
        return ByteString.copyFrom(buffer.array());
    }

    private static long durationMillis(Duration duration) {
        long secondsMillis = Math.multiplyExact(duration.getSeconds(), 1_000L);
        return Math.addExact(secondsMillis, duration.getNanos() / 1_000_000L);
    }

    /** Builder for one automatically reconnecting profile-neutral session. */
    public static final class Builder {
        private final ControllerId controllerId;
        private final URI endpoint;
        private final ProfileReference profile;
        private ProfilePayload initialProfileState = ProfilePayload.empty();
        private Executor callbackExecutor = ForkJoinPool.commonPool();
        private TlsConfig tlsConfig;
        private CoreTransport.Factory transportFactory;
        private SessionScheduler scheduler;
        private Random random;

        private Builder(
                ControllerId controllerId, URI endpoint, ProfileReference profile) {
            this.controllerId = Objects.requireNonNull(controllerId, "controllerId");
            this.endpoint = validateEndpoint(endpoint);
            this.profile = Objects.requireNonNull(profile, "profile");
        }

        /**
         * Sets the complete profile state included in the opening snapshot.
         *
         * @param state complete encoded initial state
         * @return this builder
         */
        public Builder initialProfileState(ProfilePayload state) {
            initialProfileState = Objects.requireNonNull(state, "state");
            return this;
        }

        /**
         * Selects the executor on which all public listeners are serialized.
         *
         * @param executor callback executor
         * @return this builder
         */
        public Builder callbackExecutor(Executor executor) {
            callbackExecutor = Objects.requireNonNull(executor, "executor");
            return this;
        }

        /**
         * Enables server-authenticated TLS.
         *
         * @param config server trust configuration
         * @return this builder
         */
        public Builder tls(TlsConfig config) {
            tlsConfig = Objects.requireNonNull(config, "config");
            return this;
        }

        /**
         * Builds a session without opening its transport.
         *
         * @return new session in {@link ControllerSessionState#NEW}
         */
        public CoreSession build() {
            if (tlsConfig == null && !"http".equalsIgnoreCase(endpoint.getScheme())) {
                throw new IllegalArgumentException(
                        "plaintext controller endpoints must use http");
            }
            if (tlsConfig != null && !"https".equalsIgnoreCase(endpoint.getScheme())) {
                throw new IllegalArgumentException("TLS controller endpoints must use https");
            }
            return new CoreSession(this);
        }

        Builder transportFactory(CoreTransport.Factory factory) {
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
                throw new IllegalArgumentException(
                        "controller endpoint must contain a host");
            }
            if (endpoint.getUserInfo() != null
                    || endpoint.getQuery() != null
                    || endpoint.getFragment() != null) {
                throw new IllegalArgumentException(
                        "controller endpoint must not contain credentials, query, or fragment");
            }
            return endpoint;
        }
    }
}
