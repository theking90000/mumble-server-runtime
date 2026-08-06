package io.github.theking90000.mumbleserverruntime.controller;

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

/** A local participant registration whose remote ownership may be suspended or revoked. */
public final class ParticipantHandle {
    private final ControllerSession session;
    private final ParticipantId participantId;
    private final ByteString registrationId;
    private final CopyOnWriteArrayList<ParticipantListener> listeners =
            new CopyOnWriteArrayList<ParticipantListener>();
    private final CompletableFuture<Void> firstOwnership = new CompletableFuture<Void>();
    private final NavigableMap<Long, CompletableFuture<AcceptedRevision>> pendingSpecs =
            new TreeMap<Long, CompletableFuture<AcceptedRevision>>();
    private ParticipantHandleState state = ParticipantHandleState.ACQUIRING;
    private ParticipantSpec desiredSpec;
    private ParticipantStatus latestStatus;
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

    public ParticipantId participantId() {
        return participantId;
    }

    public ParticipantHandleState state() {
        synchronized (session.monitor()) {
            return state;
        }
    }

    public ParticipantSpec desiredSpec() {
        synchronized (session.monitor()) {
            return desiredSpec;
        }
    }

    public Optional<ParticipantStatus> latestStatus() {
        synchronized (session.monitor()) {
            return Optional.ofNullable(latestStatus);
        }
    }

    public CompletableFuture<Void> whenOwned() {
        return firstOwnership;
    }

    public CompletableFuture<AcceptedRevision> setSpec(ParticipantSpec spec) {
        return session.setParticipantSpec(this, Objects.requireNonNull(spec, "spec"));
    }

    public CompletableFuture<Void> unregister() {
        return session.unregisterParticipant(this);
    }

    public void addListener(ParticipantListener listener) {
        listeners.add(Objects.requireNonNull(listener, "listener"));
    }

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
            long acceptedClientRevision,
            long acceptedRevision,
            long appliedRevision,
            long publishedGeneration) {
        ownershipToken = token;
        acceptedClientSpecRevision = maxUnsigned(acceptedClientSpecRevision, acceptedClientRevision);
        if (state != ParticipantHandleState.CLOSED) {
            changeState(ParticipantHandleState.OWNED);
            firstOwnership.complete(null);
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
        changeState(ParticipantHandleState.REVOKED);
        firstOwnership.completeExceptionally(failure);
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
        firstOwnership.completeExceptionally(failure);
        failPendingSpecs(failure);
        changeState(ParticipantHandleState.CLOSED);
        return unregisterFuture;
    }

    CompletableFuture<Void> unregisterFuture() {
        return unregisterFuture;
    }

    void releaseAcknowledged() {
        ownershipToken = ByteString.EMPTY;
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
