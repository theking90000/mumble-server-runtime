package be.theking90000.mumble.controller;

import com.google.protobuf.ByteString;
import java.util.ArrayList;
import java.util.Iterator;
import java.util.List;
import java.util.Map;
import java.util.NavigableMap;
import java.util.Objects;
import java.util.Optional;
import java.util.TreeMap;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CopyOnWriteArrayList;

/**
 * A local participant registration whose remote ownership may be suspended or revoked.
 *
 * <p>The handle retains the latest complete desired {@link ParticipantSpec}. While transport is
 * unavailable, later calls to {@link #setSpec(ParticipantSpec)} supersede earlier desired values on
 * the wire, while every pending future is completed when the runtime accepts a revision at least as
 * recent as its requested value.</p>
 *
 * <p>Revocation and local unregistration are terminal for this object. Expressing a new acquisition
 * intent requires {@link ControllerSession#registerParticipant(ParticipantId, ParticipantSpec)} and
 * produces a different handle.</p>
 */
public final class ParticipantHandle {
    private final ControllerSession session;
    private final ParticipantId participantId;
    private final ByteString registrationId;
    private final CopyOnWriteArrayList<ParticipantListener> listeners =
            new CopyOnWriteArrayList<ParticipantListener>();
    private final CompletableFuture<Void> firstOwnership = new CompletableFuture<Void>();
    private final CompletableFuture<ConnectionCredential> firstConnectionCredential =
            new CompletableFuture<ConnectionCredential>();
    private final NavigableMap<Long, CompletableFuture<AcceptedRevision>> pendingSpecs =
            new TreeMap<Long, CompletableFuture<AcceptedRevision>>();
    private ParticipantHandleState state = ParticipantHandleState.ACQUIRING;
    private ParticipantSpec desiredSpec;
    private ParticipantStatus latestStatus;
    private ConnectionCredential connectionCredential;
    private ByteString ownershipToken = ByteString.EMPTY;
    private long clientSpecRevision = 1L;
    private long acceptedClientSpecRevision;
    private CompletableFuture<Void> unregisterFuture;

    ParticipantHandle(
            ControllerSession session,
            ParticipantId participantId,
            ByteString registrationId,
            ParticipantSpec desiredSpec) {
        this.session = Objects.requireNonNull(session, "session");
        this.participantId = Objects.requireNonNull(participantId, "participantId");
        this.registrationId = Objects.requireNonNull(registrationId, "registrationId");
        this.desiredSpec = Objects.requireNonNull(desiredSpec, "desiredSpec");
    }

    /**
     * Returns the stable identity represented by this handle.
     *
     * @return participant identity
     */
    public ParticipantId participantId() {
        return participantId;
    }

    /**
     * Returns the current local ownership lifecycle state.
     *
     * @return current handle state
     */
    public ParticipantHandleState state() {
        synchronized (session.monitor()) {
            return state;
        }
    }

    /**
     * Returns the latest complete desired specification.
     *
     * <p>This value changes immediately when {@link #setSpec(ParticipantSpec)} is called, before the
     * runtime accepts or applies it.</p>
     *
     * @return latest desired specification
     */
    public ParticipantSpec desiredSpec() {
        synchronized (session.monitor()) {
            return desiredSpec;
        }
    }

    /**
     * Returns the most recent complete status received from the runtime.
     *
     * @return latest observed status, or empty before the first status update
     */
    public Optional<ParticipantStatus> latestStatus() {
        synchronized (session.monitor()) {
            return Optional.ofNullable(latestStatus);
        }
    }

    /**
     * Returns a future for the first successful ownership grant.
     *
     * <p>Temporary suspension does not replace this future. It completes exceptionally if this
     * handle is revoked or unregistered before ownership is first granted.</p>
     *
     * @return stable future completed on the first transition to {@link ParticipantHandleState#OWNED}
     */
    public CompletableFuture<Void> whenOwned() {
        return firstOwnership;
    }

    /**
     * Returns the current credential for connecting this participant with a Mumble client.
     *
     * <p>The credential becomes available with an ownership grant. It remains available during a
     * temporary session suspension, may rotate after reacquisition, and is removed when this handle
     * is revoked or closed.</p>
     *
     * @return current bearer credential, or empty before ownership and after termination
     */
    public Optional<ConnectionCredential> connectionCredential() {
        synchronized (session.monitor()) {
            return Optional.ofNullable(connectionCredential);
        }
    }

    /**
     * Returns a future for the first Mumble join credential granted to this handle.
     *
     * <p>The future completes exceptionally if the handle terminates before its first grant. Later
     * rotations are reported through {@link ParticipantListener#onConnectionCredentialChanged(
     * ParticipantHandle, ConnectionCredential)} and are visible through {@link #connectionCredential()}.</p>
     *
     * @return stable future completed with the first granted credential
     */
    public CompletableFuture<ConnectionCredential> whenConnectionCredentialAvailable() {
        return firstConnectionCredential;
    }

    /**
     * Atomically replaces the complete desired participant specification.
     *
     * <p>The returned future completes when the runtime accepts this client revision or a later
     * coalesced revision. Acceptance does not prove application or Mumble publication; inspect the
     * returned {@link AcceptedRevision} and later {@link ParticipantStatus} values.</p>
     *
     * @param spec complete replacement specification
     * @return future completed with the runtime watermarks that cover this update
     * @throws NullPointerException if {@code spec} is {@code null}
     * @throws SessionClosedException if this handle is revoked, unregistered, or no longer current
     */
    public CompletableFuture<AcceptedRevision> setSpec(ParticipantSpec spec) {
        return session.setParticipantSpec(this, Objects.requireNonNull(spec, "spec"));
    }

    /**
     * Removes this registration from desired state and releases its exact ownership capability.
     *
     * <p>The operation is idempotent for this handle. A delayed release cannot detach a newer owner
     * because the capability is checked by the runtime.</p>
     *
     * @return future completed when the release is acknowledged or the handle no longer owns the
     *         participant
     * @throws SessionClosedException if the parent session is stopping or terminal
     */
    public CompletableFuture<Void> unregister() {
        return session.unregisterParticipant(this);
    }

    /**
     * Adds a listener for subsequent state, status, and ownership-loss notifications.
     *
     * @param listener listener to add
     * @throws NullPointerException if {@code listener} is {@code null}
     */
    public void addListener(ParticipantListener listener) {
        listeners.add(Objects.requireNonNull(listener, "listener"));
    }

    /**
     * Removes a previously added participant listener.
     *
     * @param listener listener to remove; a missing listener is ignored
     */
    public void removeListener(ParticipantListener listener) {
        listeners.remove(listener);
    }

    ByteString registrationId() {
        return registrationId;
    }

    ByteString ownershipToken() {
        return ownershipToken;
    }

    long clientSpecRevision() {
        return clientSpecRevision;
    }

    long acceptedClientSpecRevision() {
        return acceptedClientSpecRevision;
    }

    boolean isDesired() {
        return state != ParticipantHandleState.REVOKED && state != ParticipantHandleState.CLOSED;
    }

    CompletableFuture<AcceptedRevision> replaceDesiredSpec(ParticipantSpec spec) {
        ensureMutable();
        desiredSpec = spec;
        clientSpecRevision++;
        CompletableFuture<AcceptedRevision> future = new CompletableFuture<AcceptedRevision>();
        pendingSpecs.put(clientSpecRevision, future);
        return future;
    }

    void ownershipGranted(
            ByteString token,
            String credential,
            long acceptedClientRevision,
            long acceptedRevision,
            long appliedRevision,
            long publishedGeneration) {
        ownershipToken = token;
        acceptedClientSpecRevision = maxUnsigned(acceptedClientSpecRevision, acceptedClientRevision);
        if (state != ParticipantHandleState.CLOSED) {
            changeState(ParticipantHandleState.OWNED);
            firstOwnership.complete(null);
            updateConnectionCredential(new ConnectionCredential(credential));
        }
        completeSpecsThrough(new AcceptedRevision(
                acceptedClientRevision,
                acceptedRevision,
                appliedRevision,
                publishedGeneration));
    }

    void specAccepted(AcceptedRevision revision) {
        acceptedClientSpecRevision = maxUnsigned(acceptedClientSpecRevision, revision.clientSpecRevision());
        completeSpecsThrough(revision);
    }

    void specRejected(long revision, Throwable failure) {
        CompletableFuture<AcceptedRevision> future = pendingSpecs.remove(revision);
        if (future != null) {
            future.completeExceptionally(failure);
        }
    }

    void suspend() {
        if (state == ParticipantHandleState.OWNED) {
            changeState(ParticipantHandleState.SUSPENDED);
        }
    }

    void revoke(String reason) {
        if (state == ParticipantHandleState.REVOKED || state == ParticipantHandleState.CLOSED) {
            if (unregisterFuture != null) {
                unregisterFuture.complete(null);
            }
            return;
        }
        OwnershipLostException failure = new OwnershipLostException(participantId, reason);
        connectionCredential = null;
        changeState(ParticipantHandleState.REVOKED);
        firstOwnership.completeExceptionally(failure);
        firstConnectionCredential.completeExceptionally(failure);
        failPendingSpecs(failure);
        for (final ParticipantListener listener : listeners) {
            session.dispatch(new Runnable() {
                @Override
                public void run() {
                    listener.onOwnershipLost(ParticipantHandle.this, reason);
                }
            });
        }
        if (unregisterFuture != null) {
            unregisterFuture.complete(null);
        }
    }

    CompletableFuture<Void> beginUnregister() {
        if (unregisterFuture != null) {
            return unregisterFuture;
        }
        unregisterFuture = new CompletableFuture<Void>();
        SessionClosedException failure = new SessionClosedException(
                "participant " + participantId + " was unregistered");
        connectionCredential = null;
        firstOwnership.completeExceptionally(failure);
        firstConnectionCredential.completeExceptionally(failure);
        failPendingSpecs(failure);
        changeState(ParticipantHandleState.CLOSED);
        return unregisterFuture;
    }

    CompletableFuture<Void> unregisterFuture() {
        return unregisterFuture;
    }

    void releaseAcknowledged() {
        ownershipToken = ByteString.EMPTY;
        connectionCredential = null;
        if (unregisterFuture != null) {
            unregisterFuture.complete(null);
        }
    }

    void updateStatus(ParticipantStatus status) {
        latestStatus = status;
        for (final ParticipantListener listener : listeners) {
            session.dispatch(new Runnable() {
                @Override
                public void run() {
                    listener.onStatusChanged(ParticipantHandle.this, status);
                }
            });
        }
    }

    private void ensureMutable() {
        if (state == ParticipantHandleState.REVOKED || state == ParticipantHandleState.CLOSED) {
            throw new SessionClosedException("participant handle is " + state);
        }
    }

    private void completeSpecsThrough(AcceptedRevision accepted) {
        List<CompletableFuture<AcceptedRevision>> completed =
                new ArrayList<CompletableFuture<AcceptedRevision>>();
        Iterator<Map.Entry<Long, CompletableFuture<AcceptedRevision>>> iterator =
                pendingSpecs.entrySet().iterator();
        while (iterator.hasNext()) {
            Map.Entry<Long, CompletableFuture<AcceptedRevision>> entry = iterator.next();
            if (Long.compareUnsigned(entry.getKey(), accepted.clientSpecRevision()) <= 0) {
                completed.add(entry.getValue());
                iterator.remove();
            }
        }
        for (CompletableFuture<AcceptedRevision> future : completed) {
            future.complete(accepted);
        }
    }

    private void failPendingSpecs(Throwable failure) {
        for (CompletableFuture<AcceptedRevision> future : pendingSpecs.values()) {
            future.completeExceptionally(failure);
        }
        pendingSpecs.clear();
    }

    private void updateConnectionCredential(final ConnectionCredential token) {
        if (connectionCredential != null && connectionCredential.value().equals(token.value())) {
            return;
        }
        connectionCredential = token;
        firstConnectionCredential.complete(token);
        for (final ParticipantListener listener : listeners) {
            session.dispatch(new Runnable() {
                @Override
                public void run() {
                    listener.onConnectionCredentialChanged(ParticipantHandle.this, token);
                }
            });
        }
    }

    private void changeState(ParticipantHandleState next) {
        if (state == next) {
            return;
        }
        final ParticipantHandleState previous = state;
        state = next;
        for (final ParticipantListener listener : listeners) {
            session.dispatch(new Runnable() {
                @Override
                public void run() {
                    listener.onStateChanged(ParticipantHandle.this, previous, next);
                }
            });
        }
    }

    private static long maxUnsigned(long first, long second) {
        return Long.compareUnsigned(first, second) >= 0 ? first : second;
    }
}
