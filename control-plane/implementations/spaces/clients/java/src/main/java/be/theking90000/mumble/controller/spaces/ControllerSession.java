package be.theking90000.mumble.controller.spaces;

import be.theking90000.mumble.controller.core.CommandRejectedException;
import be.theking90000.mumble.controller.core.ControllerException;
import be.theking90000.mumble.controller.core.ControllerId;
import be.theking90000.mumble.controller.core.ControllerSessionState;
import be.theking90000.mumble.controller.core.CoreParticipantHandle;
import be.theking90000.mumble.controller.core.CoreSession;
import be.theking90000.mumble.controller.core.CoreSessionListener;
import be.theking90000.mumble.controller.core.ParticipantId;
import be.theking90000.mumble.controller.core.ProfileEventListener;
import be.theking90000.mumble.controller.core.ProfilePayload;
import be.theking90000.mumble.controller.core.ProfileReference;
import be.theking90000.mumble.controller.core.TlsConfig;

import be.theking90000.mumble.controller.spaces.internal.ProfileMetadata;
import be.theking90000.mumble.controller.internal.spaces.v1.Command;
import be.theking90000.mumble.controller.internal.spaces.v1.Event;
import be.theking90000.mumble.controller.internal.spaces.v1.FetchSpace;
import be.theking90000.mumble.controller.internal.spaces.v1.FetchSpaceResult;
import be.theking90000.mumble.controller.internal.spaces.v1.ObservedSpacesAccepted;
import be.theking90000.mumble.controller.internal.spaces.v1.ReplaceObservedSpaces;
import java.net.URI;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Objects;
import java.util.Optional;
import java.util.Random;
import java.util.Set;
import java.util.TreeSet;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.Executor;
import java.util.concurrent.ForkJoinPool;

/**
 * Typed Spaces facade over the profile-neutral {@link CoreSession}.
 *
 * <p>This class owns only Spaces payload encoding, explicit observations, and the decoded space
 * cache. The delegated Core session owns transport lifecycle, leases, reconciliation, reliable
 * requests, participant ownership, revisions, and reconnection.</p>
 */
public final class ControllerSession {
    private final Object monitor = new Object();
    private final Object observationOperations = new Object();
    private final CoreSession core;
    private final Map<ParticipantId, ParticipantHandle> participants =
            new LinkedHashMap<ParticipantId, ParticipantHandle>();
    private final Set<SpaceKey> explicitlyObservedSpaces = new TreeSet<SpaceKey>();
    private final Map<SpaceKey, SpaceSnapshot> spaces =
            new LinkedHashMap<SpaceKey, SpaceSnapshot>();
    private final CopyOnWriteArrayList<ControllerSessionListener> sessionListeners =
            new CopyOnWriteArrayList<ControllerSessionListener>();
    private final CopyOnWriteArrayList<SpaceListener> spaceListeners =
            new CopyOnWriteArrayList<SpaceListener>();
    private long observedSpacesRevision;
    private CompletableFuture<Void> latestObservation =
            CompletableFuture.completedFuture(null);

    private ControllerSession(Builder builder) {
        ProfileReference profile = ProfileReference.of(
                ProfileMetadata.SPACES_PROFILE_ID,
                ProfileMetadata.SPACES_SCHEMA_VERSION,
                ProfileMetadata.SPACES_DESCRIPTOR_DIGEST);
        if (builder.coreSession != null) {
            core = builder.coreSession;
        } else {
            CoreSession.Builder coreBuilder = CoreSession.builder(
                        builder.controllerId, builder.endpoint, profile)
                .initialProfileState(SpacesWire.desiredState(explicitlyObservedSpaces))
                .callbackExecutor(builder.callbackExecutor);
            if (builder.tlsConfig != null) {
                coreBuilder.tls(builder.tlsConfig);
            }
            core = coreBuilder.build();
        }
        core.addSessionListener(new CoreSessionListener() {
            @Override
            public void onStateChanged(
                    CoreSession ignored,
                    ControllerSessionState previous,
                    ControllerSessionState current) {
                if (current == ControllerSessionState.RECONNECTING
                        || current == ControllerSessionState.FAILED
                        || current == ControllerSessionState.CLOSED) {
                    clearSpaceCache();
                }
                if (current == ControllerSessionState.FAILED
                        || current == ControllerSessionState.CLOSED) {
                    synchronized (monitor) {
                        participants.clear();
                    }
                }
                for (ControllerSessionListener listener : sessionListeners) {
                    listener.onStateChanged(ControllerSession.this, previous, current);
                }
            }
        });
        core.addProfileEventListener(new ProfileEventListener() {
            @Override
            public void onProfileEvent(CoreSession ignored, ProfilePayload payload) {
                try {
                    acceptEvent(SpacesWire.event(payload));
                } catch (ControllerException failure) {
                    core.reportProfileProtocolFailure(failure);
                }
            }
        });
    }

    /**
     * Creates a Spaces session builder.
     *
     * @param controllerId stable declarative controller identity
     * @param endpoint Controller service URI
     * @return new builder
     */
    public static Builder builder(ControllerId controllerId, URI endpoint) {
        return new Builder(controllerId, endpoint);
    }

    /** @return declarative controller identity */
    public ControllerId controllerId() {
        return core.controllerId();
    }

    /** @return current session lifecycle state */
    public ControllerSessionState state() {
        return core.state();
    }

    /** @return future completed after opening reconciliation */
    public CompletableFuture<Void> start() {
        return core.start();
    }

    /**
     * Registers a participant with a complete Spaces specification.
     *
     * @param participantId stable participant identity
     * @param spec complete desired Spaces state
     * @return typed participant handle
     */
    public ParticipantHandle registerParticipant(
            ParticipantId participantId, ParticipantSpec spec) {
        Objects.requireNonNull(participantId, "participantId");
        Objects.requireNonNull(spec, "spec");
        CoreParticipantHandle coreHandle = core.registerParticipant(
                participantId, SpacesWire.participantSpec(spec));
        ParticipantHandle handle = new ParticipantHandle(this, coreHandle, spec);
        synchronized (monitor) {
            participants.put(participantId, handle);
        }
        return handle;
    }

    /**
     * Looks up a current desired participant.
     *
     * @param participantId identity to find
     * @return typed handle, or empty after release or revocation
     */
    public Optional<ParticipantHandle> participant(ParticipantId participantId) {
        Objects.requireNonNull(participantId, "participantId");
        synchronized (monitor) {
            return Optional.ofNullable(participants.get(participantId));
        }
    }

    /** @return immutable point-in-time map of desired typed participants */
    public Map<ParticipantId, ParticipantHandle> participants() {
        synchronized (monitor) {
            Map<ParticipantId, ParticipantHandle> snapshot =
                    new LinkedHashMap<ParticipantId, ParticipantHandle>(participants);
            return Collections.unmodifiableMap(snapshot);
        }
    }

    /**
     * Adds a space to the complete explicit observation set.
     *
     * @param spaceKey semantic key to observe
     * @return future completed when the change is accepted
     */
    public CompletableFuture<Void> observeSpace(SpaceKey spaceKey) {
        return setSpaceObservation(Objects.requireNonNull(spaceKey, "spaceKey"), true);
    }

    /**
     * Removes a space from the explicit observation set.
     *
     * @param spaceKey semantic key to stop observing
     * @return future completed when the change is accepted
     */
    public CompletableFuture<Void> unobserveSpace(SpaceKey spaceKey) {
        return setSpaceObservation(Objects.requireNonNull(spaceKey, "spaceKey"), false);
    }

    /** @return immutable point-in-time copy of explicit observation keys */
    public Set<SpaceKey> explicitlyObservedSpaces() {
        synchronized (monitor) {
            return Collections.unmodifiableSet(new TreeSet<SpaceKey>(explicitlyObservedSpaces));
        }
    }

    /** @return immutable point-in-time copy of the latest decoded space cache */
    public Map<SpaceKey, SpaceSnapshot> spaces() {
        synchronized (monitor) {
            return Collections.unmodifiableMap(
                    new LinkedHashMap<SpaceKey, SpaceSnapshot>(spaces));
        }
    }

    /**
     * Fetches one full snapshot without changing the observation set.
     *
     * @param spaceKey semantic key to fetch
     * @return future completed with the one-shot snapshot
     */
    public CompletableFuture<SpaceSnapshot> fetchSpace(SpaceKey spaceKey) {
        Objects.requireNonNull(spaceKey, "spaceKey");
        Command command = Command.newBuilder()
                .setFetchSpace(FetchSpace.newBuilder().setSpaceKey(spaceKey.value()))
                .build();
        return core.sendProfileCommand(SpacesWire.command(command)).thenApply(payload -> {
            Event event = SpacesWire.event(payload);
            if (event.getEventCase() != Event.EventCase.FETCH_SPACE_RESULT) {
                throw new ControllerException(
                        "Spaces fetch returned " + event.getEventCase());
            }
            return acceptFetch(event.getFetchSpaceResult());
        });
    }

    /** @return future completed when the delegated Core session is locally closed */
    public CompletableFuture<Void> stop() {
        return core.stop();
    }

    /**
     * Adds a listener for subsequent session transitions.
     *
     * @param listener listener to add
     */
    public void addSessionListener(ControllerSessionListener listener) {
        sessionListeners.add(Objects.requireNonNull(listener, "listener"));
    }

    /**
     * Removes a session listener; a missing listener is ignored.
     *
     * @param listener listener to remove
     */
    public void removeSessionListener(ControllerSessionListener listener) {
        sessionListeners.remove(listener);
    }

    /**
     * Adds a listener for subsequent full space replacements and closures.
     *
     * @param listener listener to add
     */
    public void addSpaceListener(SpaceListener listener) {
        spaceListeners.add(Objects.requireNonNull(listener, "listener"));
    }

    /**
     * Removes a space listener; a missing listener is ignored.
     *
     * @param listener listener to remove
     */
    public void removeSpaceListener(SpaceListener listener) {
        spaceListeners.remove(listener);
    }

    Object monitor() {
        return monitor;
    }

    void removeParticipant(ParticipantHandle handle) {
        synchronized (monitor) {
            participants.remove(handle.participantId(), handle);
        }
    }

    private CompletableFuture<Void> setSpaceObservation(SpaceKey spaceKey, boolean observe) {
        synchronized (observationOperations) {
            final long revision;
            final Set<SpaceKey> desiredSpaces;
            synchronized (monitor) {
                boolean changed = observe
                        ? explicitlyObservedSpaces.add(spaceKey)
                        : explicitlyObservedSpaces.remove(spaceKey);
                if (!changed) {
                    return latestObservation;
                }
                observedSpacesRevision++;
                revision = observedSpacesRevision;
                desiredSpaces = new TreeSet<SpaceKey>(explicitlyObservedSpaces);
            }
            boolean active = core.state() == ControllerSessionState.ACTIVE;
            ProfilePayload resultingState = SpacesWire.desiredState(desiredSpaces);
            CompletableFuture<ProfilePayload> commandResult = null;
            if (active) {
                ReplaceObservedSpaces replace = ReplaceObservedSpaces.newBuilder()
                        .setObservedSpacesRevision(revision)
                        .addAllSpaceKeys(spaceValues(desiredSpaces))
                        .build();
                commandResult = core.sendProfileCommand(
                        SpacesWire.command(
                                Command.newBuilder().setReplaceObservedSpaces(replace).build()),
                        resultingState);
            }
            CompletableFuture<Void> operation;
            if (commandResult == null) {
                operation = core.setProfileState(resultingState);
            } else {
                operation = commandResult.thenAccept(payload ->
                        acceptObservation(revision, SpacesWire.event(payload)));
            }
            synchronized (monitor) {
                latestObservation = operation;
            }
            return operation;
        }
    }

    private void acceptObservation(long requestedRevision, Event event) {
        if (event.getEventCase() != Event.EventCase.OBSERVED_SPACES_ACCEPTED) {
            throw new ControllerException(
                    "Spaces observation returned " + event.getEventCase());
        }
        ObservedSpacesAccepted accepted = event.getObservedSpacesAccepted();
        if (Long.compareUnsigned(
                accepted.getObservedSpacesRevision(), requestedRevision) < 0) {
            throw new ControllerException("Spaces accepted a stale observation revision");
        }
    }

    private void acceptEvent(Event event) {
        switch (event.getEventCase()) {
            case SPACE_SNAPSHOT:
                acceptSpaceSnapshot(event.getSpaceSnapshot());
                break;
            case SPACE_CLOSED:
                acceptSpaceClosed(event.getSpaceClosed());
                break;
            case OBSERVED_SPACES_ACCEPTED:
                final long currentObservationRevision;
                synchronized (monitor) {
                    currentObservationRevision = observedSpacesRevision;
                }
                if (Long.compareUnsigned(
                        event.getObservedSpacesAccepted().getObservedSpacesRevision(),
                        currentObservationRevision) > 0) {
                    core.reportProfileProtocolFailure(new ControllerException(
                            "Spaces accepted an unknown observation revision"));
                }
                break;
            case FETCH_SPACE_RESULT:
                core.reportProfileProtocolFailure(new ControllerException(
                        "correlated Spaces response has no matching request"));
                break;
            case EVENT_NOT_SET:
            default:
                core.reportProfileProtocolFailure(new ControllerException(
                        "Spaces event has no supported payload"));
                break;
        }
    }

    private void acceptSpaceSnapshot(
            be.theking90000.mumble.controller.internal.spaces.v1.SpaceSnapshot wire) {
        final SpaceSnapshot snapshot = fromProtocolSpace(wire);
        synchronized (monitor) {
            SpaceSnapshot current = spaces.get(snapshot.spaceKey());
            if (current != null
                    && current.incarnation().equals(snapshot.incarnation())
                    && Long.compareUnsigned(
                            current.spaceRevision(), snapshot.spaceRevision()) >= 0) {
                return;
            }
            spaces.put(snapshot.spaceKey(), snapshot);
        }
        for (SpaceListener listener : spaceListeners) {
            listener.onSpaceUpdated(this, snapshot);
        }
    }

    private void acceptSpaceClosed(
            be.theking90000.mumble.controller.internal.spaces.v1.SpaceClosed wire) {
        final SpaceKey spaceKey = SpaceKey.of(wire.getSpaceKey());
        final SpaceIncarnation incarnation =
                new SpaceIncarnation(wire.getIncarnationId().toByteArray());
        synchronized (monitor) {
            SpaceSnapshot current = spaces.get(spaceKey);
            if (current == null || !current.incarnation().equals(incarnation)) {
                return;
            }
            spaces.remove(spaceKey);
        }
        for (SpaceListener listener : spaceListeners) {
            listener.onSpaceClosed(
                    this, spaceKey, incarnation, wire.getFinalSpaceRevision());
        }
    }

    private static SpaceSnapshot acceptFetch(FetchSpaceResult result) {
        switch (result.getResultCase()) {
            case SNAPSHOT:
                return fromProtocolSpace(result.getSnapshot());
            case ABSENT:
                throw new CommandRejectedException(
                        "NOT_FOUND",
                        "space is not materialized: " + result.getAbsent().getSpaceKey());
            case RESULT_NOT_SET:
            default:
                throw new ControllerException("FetchSpaceResult has no result");
        }
    }

    private void clearSpaceCache() {
        synchronized (monitor) {
            spaces.clear();
        }
    }

    private static Iterable<String> spaceValues(Set<SpaceKey> keys) {
        java.util.List<String> values = new java.util.ArrayList<String>();
        for (SpaceKey key : keys) {
            values.add(key.value());
        }
        return values;
    }

    private static SpaceSnapshot fromProtocolSpace(
            be.theking90000.mumble.controller.internal.spaces.v1.SpaceSnapshot wire) {
        java.util.List<SpaceParticipant> participants =
                new java.util.ArrayList<SpaceParticipant>();
        for (be.theking90000.mumble.controller.internal.spaces.v1.SpaceParticipant participant
                : wire.getParticipantsList()) {
            participants.add(new SpaceParticipant(
                    ParticipantId.of(participant.getParticipantId()),
                    participant.getDisplayName(),
                    participant.getServerMute(),
                    participant.getServerDeaf(),
                    participant.getConnected()));
        }
        return new SpaceSnapshot(
                SpaceKey.of(wire.getSpaceKey()),
                new SpaceIncarnation(wire.getIncarnationId().toByteArray()),
                wire.getSpaceRevision(),
                participants,
                wire.getPublishedGeneration());
    }

    /** Builder for one typed Spaces session. */
    public static final class Builder {
        private final ControllerId controllerId;
        private final URI endpoint;
        private Executor callbackExecutor = ForkJoinPool.commonPool();
        private TlsConfig tlsConfig;
        private CoreSession coreSession;

        private Builder(ControllerId controllerId, URI endpoint) {
            this.controllerId = Objects.requireNonNull(controllerId, "controllerId");
            this.endpoint = Objects.requireNonNull(endpoint, "endpoint");
        }

        /**
         * Selects the executor on which public callbacks are serialized.
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

        /** @return a new typed Spaces session without opening its transport */
        public ControllerSession build() {
            return new ControllerSession(this);
        }

        Builder coreSession(CoreSession value) {
            coreSession = Objects.requireNonNull(value, "value");
            return this;
        }
    }
}
