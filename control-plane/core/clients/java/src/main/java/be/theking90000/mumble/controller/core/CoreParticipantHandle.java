package be.theking90000.mumble.controller.core;

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

/** Generic participant ownership handle maintained by {@link CoreSession}. */
public final class CoreParticipantHandle {
    private final CoreSession session;
    private final ParticipantId participantId;
    private final ByteString registrationId;
    private final CopyOnWriteArrayList<CoreParticipantListener> listeners =
            new CopyOnWriteArrayList<CoreParticipantListener>();
    private final CompletableFuture<Void> firstOwnership = new CompletableFuture<Void>();
    private final CompletableFuture<ConnectionCredential> firstConnectionCredential =
            new CompletableFuture<ConnectionCredential>();
    private final NavigableMap<Long, CompletableFuture<AcceptedRevision>> pendingSpecs =
            new TreeMap<Long, CompletableFuture<AcceptedRevision>>();
    private ParticipantHandleState state = ParticipantHandleState.ACQUIRING;
    private ProfilePayload desiredSpec;
    private CoreParticipantStatus latestStatus;
    private ConnectionCredential connectionCredential;
    private ByteString ownershipToken = ByteString.EMPTY;
    private long clientSpecRevision = 1L;
    private long acceptedClientSpecRevision;
    private CompletableFuture<Void> unregisterFuture;

    CoreParticipantHandle(
            CoreSession session,
            ParticipantId participantId,
            ByteString registrationId,
            ProfilePayload desiredSpec) {
        this.session = Objects.requireNonNull(session, "session");
        this.participantId = Objects.requireNonNull(participantId, "participantId");
        this.registrationId = Objects.requireNonNull(registrationId, "registrationId");
        this.desiredSpec = Objects.requireNonNull(desiredSpec, "desiredSpec");
    }

    /** @return stable logical participant identity */
    public ParticipantId participantId() {
        return participantId;
    }

    /** @return local ownership lifecycle state */
    public ParticipantHandleState state() {
        synchronized (session.monitor()) {
            return state;
        }
    }

    /** @return latest complete encoded desired profile specification */
    public ProfilePayload desiredSpec() {
        synchronized (session.monitor()) {
            return desiredSpec;
        }
    }

    /** @return latest generic status received from the runtime */
    public Optional<CoreParticipantStatus> latestStatus() {
        synchronized (session.monitor()) {
            return Optional.ofNullable(latestStatus);
        }
    }

    /** @return stable future completed on the first ownership grant */
    public CompletableFuture<Void> whenOwned() {
        return firstOwnership;
    }

    /** @return current connection credential, if ownership has granted one */
    public Optional<ConnectionCredential> connectionCredential() {
        synchronized (session.monitor()) {
            return Optional.ofNullable(connectionCredential);
        }
    }

    /** @return stable future completed with the first connection credential */
    public CompletableFuture<ConnectionCredential> whenConnectionCredentialAvailable() {
        return firstConnectionCredential;
    }

    /**
     * Replaces the complete encoded participant profile specification.
     *
     * @param spec complete encoded specification
     * @return future completed when the profile accepts this revision
     */
    public CompletableFuture<AcceptedRevision> setSpec(ProfilePayload spec) {
        return session.setParticipantSpec(this, Objects.requireNonNull(spec, "spec"));
    }

    /** @return future completed after this exact ownership capability is released */
    public CompletableFuture<Void> unregister() {
        return session.unregisterParticipant(this);
    }

    /**
     * Adds a listener for subsequent lifecycle and status notifications.
     *
     * @param listener listener to add
     */
    public void addListener(CoreParticipantListener listener) {
        listeners.add(Objects.requireNonNull(listener, "listener"));
    }

    /**
     * Removes a listener; a missing listener is ignored.
     *
     * @param listener listener to remove
     */
    public void removeListener(CoreParticipantListener listener) {
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

    CompletableFuture<AcceptedRevision> replaceDesiredSpec(ProfilePayload spec) {
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
        acceptedClientSpecRevision = maxUnsigned(
                acceptedClientSpecRevision, revision.clientSpecRevision());
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
        for (final CoreParticipantListener listener : listeners) {
            session.dispatch(new Runnable() {
                @Override
                public void run() {
                    listener.onOwnershipLost(CoreParticipantHandle.this, reason);
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

    void updateStatus(CoreParticipantStatus status) {
        latestStatus = status;
        for (final CoreParticipantListener listener : listeners) {
            session.dispatch(new Runnable() {
                @Override
                public void run() {
                    listener.onStatusChanged(CoreParticipantHandle.this, status);
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

    private void updateConnectionCredential(final ConnectionCredential credential) {
        if (connectionCredential != null
                && connectionCredential.value().equals(credential.value())) {
            return;
        }
        connectionCredential = credential;
        firstConnectionCredential.complete(credential);
        for (final CoreParticipantListener listener : listeners) {
            session.dispatch(new Runnable() {
                @Override
                public void run() {
                    listener.onConnectionCredentialChanged(
                            CoreParticipantHandle.this, credential);
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
        for (final CoreParticipantListener listener : listeners) {
            session.dispatch(new Runnable() {
                @Override
                public void run() {
                    listener.onStateChanged(CoreParticipantHandle.this, previous, next);
                }
            });
        }
    }

    private static long maxUnsigned(long first, long second) {
        return Long.compareUnsigned(first, second) >= 0 ? first : second;
    }
}
