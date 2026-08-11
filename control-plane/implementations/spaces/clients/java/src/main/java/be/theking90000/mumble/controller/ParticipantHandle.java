package be.theking90000.mumble.controller;

import com.google.protobuf.ByteString;
import java.util.Objects;
import java.util.Optional;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CopyOnWriteArrayList;

/** Typed Spaces view of a profile-neutral participant ownership handle. */
public final class ParticipantHandle {
    private final ControllerSession session;
    private final CoreParticipantHandle delegate;
    private final CopyOnWriteArrayList<ParticipantListener> listeners =
            new CopyOnWriteArrayList<ParticipantListener>();
    private ParticipantSpec desiredSpec;
    private ParticipantStatus latestStatus;

    ParticipantHandle(
            ControllerSession session,
            CoreParticipantHandle delegate,
            ParticipantSpec desiredSpec) {
        this.session = Objects.requireNonNull(session, "session");
        this.delegate = Objects.requireNonNull(delegate, "delegate");
        this.desiredSpec = Objects.requireNonNull(desiredSpec, "desiredSpec");
        delegate.addListener(new CoreParticipantListener() {
            @Override
            public void onStateChanged(
                    CoreParticipantHandle ignored,
                    ParticipantHandleState previous,
                    ParticipantHandleState current) {
                for (ParticipantListener listener : listeners) {
                    listener.onStateChanged(ParticipantHandle.this, previous, current);
                }
            }

            @Override
            public void onStatusChanged(
                    CoreParticipantHandle ignored, CoreParticipantStatus status) {
                ParticipantStatus decoded = decodeStatus(status);
                synchronized (ParticipantHandle.this.session.monitor()) {
                    latestStatus = decoded;
                }
                for (ParticipantListener listener : listeners) {
                    listener.onStatusChanged(ParticipantHandle.this, decoded);
                }
            }

            @Override
            public void onConnectionCredentialChanged(
                    CoreParticipantHandle ignored, ConnectionCredential credential) {
                for (ParticipantListener listener : listeners) {
                    listener.onConnectionCredentialChanged(
                            ParticipantHandle.this, credential);
                }
            }

            @Override
            public void onOwnershipLost(CoreParticipantHandle ignored, String reason) {
                ParticipantHandle.this.session.removeParticipant(ParticipantHandle.this);
                for (ParticipantListener listener : listeners) {
                    listener.onOwnershipLost(ParticipantHandle.this, reason);
                }
            }
        });
    }

    /** @return stable logical participant identity */
    public ParticipantId participantId() {
        return delegate.participantId();
    }

    /** @return current local ownership lifecycle state */
    public ParticipantHandleState state() {
        return delegate.state();
    }

    /** @return latest complete desired Spaces specification */
    public ParticipantSpec desiredSpec() {
        synchronized (session.monitor()) {
            return desiredSpec;
        }
    }

    /** @return latest decoded Spaces status, or empty before the first update */
    public Optional<ParticipantStatus> latestStatus() {
        synchronized (session.monitor()) {
            return Optional.ofNullable(latestStatus);
        }
    }

    /** @return stable future completed on the first ownership grant */
    public CompletableFuture<Void> whenOwned() {
        return delegate.whenOwned();
    }

    /** @return current connection credential, if ownership has granted one */
    public Optional<ConnectionCredential> connectionCredential() {
        return delegate.connectionCredential();
    }

    /** @return stable future completed with the first connection credential */
    public CompletableFuture<ConnectionCredential> whenConnectionCredentialAvailable() {
        return delegate.whenConnectionCredentialAvailable();
    }

    /**
     * Replaces the complete desired Spaces participant specification.
     *
     * @param spec complete replacement specification
     * @return future completed with the acceptance watermarks covering this update
     */
    public CompletableFuture<AcceptedRevision> setSpec(ParticipantSpec spec) {
        Objects.requireNonNull(spec, "spec");
        synchronized (session.monitor()) {
            desiredSpec = spec;
        }
        return delegate.setSpec(SpacesWire.participantSpec(spec));
    }

    /** @return future completed after this exact ownership capability is released */
    public CompletableFuture<Void> unregister() {
        CompletableFuture<Void> future = delegate.unregister();
        future.whenComplete((ignored, failure) -> session.removeParticipant(this));
        return future;
    }

    /**
     * Adds a listener for subsequent typed lifecycle and status notifications.
     *
     * @param listener listener to add
     */
    public void addListener(ParticipantListener listener) {
        listeners.add(Objects.requireNonNull(listener, "listener"));
    }

    /**
     * Removes a listener; a missing listener is ignored.
     *
     * @param listener listener to remove
     */
    public void removeListener(ParticipantListener listener) {
        listeners.remove(listener);
    }

    ByteString registrationId() {
        return delegate.registrationId();
    }

    long clientSpecRevision() {
        return delegate.clientSpecRevision();
    }

    CoreParticipantHandle coreHandle() {
        return delegate;
    }

    private static ParticipantStatus decodeStatus(CoreParticipantStatus coreStatus) {
        be.theking90000.mumble.controller.internal.spaces.v1.ParticipantStatus status =
                SpacesWire.status(coreStatus);
        SpaceKey appliedSpace = status.getAppliedSpaceKey().isEmpty()
                ? null
                : SpaceKey.of(status.getAppliedSpaceKey());
        return new ParticipantStatus(
                coreStatus.connected(),
                appliedSpace,
                status.getSelfMute(),
                status.getSelfDeaf(),
                coreStatus.acceptedSpecRevision(),
                coreStatus.appliedSpecRevision(),
                coreStatus.publishedGeneration(),
                coreStatus.applicationError().orElse(""));
    }
}
