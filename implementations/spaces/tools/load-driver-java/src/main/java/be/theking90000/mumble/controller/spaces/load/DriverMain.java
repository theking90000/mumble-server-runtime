package be.theking90000.mumble.controller.spaces.load;

import be.theking90000.mumble.controller.core.AcceptedRevision;
import be.theking90000.mumble.controller.core.ConnectionCredential;
import be.theking90000.mumble.controller.core.ControllerId;
import be.theking90000.mumble.controller.core.ControllerSessionState;
import be.theking90000.mumble.controller.core.ParticipantHandleState;
import be.theking90000.mumble.controller.core.ParticipantId;
import be.theking90000.mumble.controller.spaces.ControllerSession;
import be.theking90000.mumble.controller.spaces.ControllerSessionListener;
import be.theking90000.mumble.controller.spaces.ParticipantHandle;
import be.theking90000.mumble.controller.spaces.ParticipantListener;
import be.theking90000.mumble.controller.spaces.ParticipantSpec;
import be.theking90000.mumble.controller.spaces.ParticipantStatus;
import be.theking90000.mumble.controller.spaces.SpaceKey;
import be.theking90000.mumble.controller.spaces.SpaceListener;
import be.theking90000.mumble.controller.spaces.SpaceSnapshot;

import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.io.PrintWriter;
import java.net.URI;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.Set;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.Semaphore;
import java.util.concurrent.TimeUnit;

/** Hosts one or more real Spaces SDK Controller sessions behind a bounded NDJSON pipe. */
public final class DriverMain {
    private static final int SCHEMA_VERSION = 1;
    private static final int OUTPUT_CAPACITY = 16384;
    private static final int OUTPUT_BATCH_SIZE = 256;

    private DriverMain() {
    }

    /** Entry point. Arguments: endpoint, comma-separated Controller IDs, command window, mode. */
    public static void main(String[] arguments) throws Exception {
        if (arguments.length < 2 || arguments.length > 4) {
            throw new IllegalArgumentException(
                    "usage: load-driver-java ENDPOINT CONTROLLER_IDS [MAX_IN_FLIGHT] [EVENT_MODE]");
        }
        int maximumInFlight = arguments.length >= 3 ? Integer.parseInt(arguments[2]) : 256;
        if (maximumInFlight < 1) {
            throw new IllegalArgumentException("MAX_IN_FLIGHT must be positive");
        }
        DriverEventMode eventMode = arguments.length == 4
                ? DriverEventMode.parse(arguments[3]) : DriverEventMode.FULL;
        DriverHost driver = new DriverHost(
                URI.create(arguments[0]),
                parseControllerIds(arguments[1]),
                maximumInFlight,
                eventMode);
        driver.run();
    }

    static List<ControllerId> parseControllerIds(String encoded) {
        String[] values = encoded.split(",", -1);
        Set<String> seen = new LinkedHashSet<String>();
        List<ControllerId> result = new ArrayList<ControllerId>(values.length);
        for (String value : values) {
            if (value.isEmpty()) {
                throw new IllegalArgumentException("CONTROLLER_IDS cannot contain an empty ID");
            }
            if (!seen.add(value)) {
                throw new IllegalArgumentException("duplicate Controller ID: " + value);
            }
            result.add(ControllerId.of(value));
        }
        return result;
    }

    private static final class DriverHost {
        private final long started = System.nanoTime();
        private final DriverEventMode eventMode;
        private final DriverOutput output = new DriverOutput(
                new PrintWriter(System.out, false), OUTPUT_CAPACITY, OUTPUT_BATCH_SIZE);
        private final LinkedHashMap<String, ControllerDriver> controllers =
                new LinkedHashMap<String, ControllerDriver>();

        private DriverHost(
                URI endpoint,
                List<ControllerId> controllerIds,
                int maximumInFlight,
                DriverEventMode eventMode) {
            this.eventMode = eventMode;
            for (ControllerId controllerId : controllerIds) {
                controllers.put(
                        controllerId.value(),
                        new ControllerDriver(endpoint, controllerId, maximumInFlight));
            }
        }

        private void run() throws Exception {
            output.start();
            for (ControllerDriver controller : controllers.values()) {
                controller.start();
            }
            BufferedReader input = new BufferedReader(new InputStreamReader(
                    System.in, StandardCharsets.UTF_8));
            boolean shutdown = false;
            String line;
            while (!shutdown && !output.failed() && (line = input.readLine()) != null) {
                shutdown = accept(line);
            }

            List<CompletableFuture<Void>> stops = new ArrayList<CompletableFuture<Void>>();
            for (ControllerDriver controller : controllers.values()) {
                stops.add(controller.stop());
            }
            CompletableFuture<?>[] stopArray = stops.toArray(new CompletableFuture<?>[stops.size()]);
            CompletableFuture.allOf(stopArray).get(30, TimeUnit.SECONDS);
            for (ControllerDriver controller : controllers.values()) {
                controller.shutdownCallbacks();
                emit(event("stopped", "shutdown", controller.controllerId.value(), ""));
            }
            emitOutputCounters();
            output.close(TimeUnit.SECONDS.toMillis(5));
            if (output.failed()) {
                throw new IllegalStateException("bounded output queue saturated");
            }
        }

        private boolean accept(String line) {
            String correlationId = "unparsed";
            String controllerId = "";
            try {
                JsonLine command = JsonLine.parse(line);
                correlationId = command.required("correlation_id");
                if (command.requiredInt("schema_version") != SCHEMA_VERSION) {
                    throw new IllegalArgumentException("unsupported schema_version");
                }
                String kind = command.required("kind");
                if ("shutdown".equals(kind)) {
                    emit(event("shutdown_accepted", correlationId, "", ""));
                    return true;
                }
                controllerId = command.required("controller_id");
                ControllerDriver controller = controllers.get(controllerId);
                if (controller == null) {
                    throw new IllegalArgumentException("unknown Controller ID: " + controllerId);
                }
                controller.dispatch(kind, correlationId, command);
            } catch (RuntimeException failure) {
                emit(error(correlationId, controllerId, failure));
            }
            return false;
        }

        private Map<String, Object> event(
                String kind,
                String correlationId,
                String controllerId,
                String participantId) {
            LinkedHashMap<String, Object> event = new LinkedHashMap<String, Object>();
            event.put("schema_version", SCHEMA_VERSION);
            event.put("kind", kind);
            event.put("correlation_id", correlationId);
            event.put("controller_id", controllerId);
            event.put("participant_id", participantId);
            event.put("monotonic_nanos", System.nanoTime() - started);
            return event;
        }

        private Map<String, Object> error(
                String correlationId, String controllerId, Throwable failure) {
            Map<String, Object> event = event("error", correlationId, controllerId, "");
            event.put("error_type", failure.getClass().getSimpleName());
            event.put("message", safeMessage(failure));
            return event;
        }

        private void emit(Map<String, Object> event) {
            output.emit(JsonLine.object(event));
        }

        private void emitOutputCounters() {
            DriverOutput.Snapshot snapshot = output.snapshot();
            Map<String, Object> event = event("driver_output", "shutdown", "", "");
            event.put("event_mode", eventMode.name().toLowerCase());
            event.put("controllers", controllers.size());
            event.put("enqueued", snapshot.enqueued);
            event.put("coalesced", snapshot.coalesced);
            event.put("suppressed", snapshot.suppressed);
            event.put("written", snapshot.written);
            event.put("batches", snapshot.batches);
            emit(event);
        }

        private final class ControllerDriver {
            private final ControllerId controllerId;
            private final ExecutorService callbacks = Executors.newSingleThreadExecutor();
            private final ConcurrentHashMap<ParticipantId, ParticipantHandle> participants =
                    new ConcurrentHashMap<ParticipantId, ParticipantHandle>();
            private final Semaphore operations;
            private final ControllerSession session;

            private ControllerDriver(
                    URI endpoint, ControllerId controllerId, int maximumInFlight) {
                this.controllerId = controllerId;
                operations = new Semaphore(maximumInFlight);
                session = ControllerSession.builder(controllerId, endpoint)
                        .callbackExecutor(callbacks)
                        .build();
                installListeners();
            }

            private void start() {
                session.start().whenComplete((ignored, failure) -> {
                    if (failure == null) {
                        emit(event("started", "startup", ""));
                    } else {
                        emit(error("startup", failure));
                    }
                });
            }

            private CompletableFuture<Void> stop() {
                return session.stop();
            }

            private void shutdownCallbacks() throws InterruptedException {
                callbacks.shutdown();
                callbacks.awaitTermination(30, TimeUnit.SECONDS);
            }

            private void dispatch(String kind, String correlationId, JsonLine command) {
                if (!operations.tryAcquire()) {
                    emit(error(correlationId, new IllegalStateException(
                            "operation window saturated")));
                    return;
                }
                try {
                    if ("register".equals(kind)) {
                        register(correlationId, command);
                    } else if ("set_spec".equals(kind)) {
                        setSpec(correlationId, command);
                    } else if ("release".equals(kind)) {
                        release(correlationId, command);
                    } else if ("observe_space".equals(kind)) {
                        track(correlationId, "", session.observeSpace(
                                SpaceKey.of(command.required("space_key"))), "space_observed");
                    } else if ("fetch_space".equals(kind)) {
                        fetchSpace(correlationId, command);
                    } else {
                        throw new IllegalArgumentException("unsupported command kind: " + kind);
                    }
                } catch (RuntimeException failure) {
                    operations.release();
                    throw failure;
                }
            }

            private void register(String correlationId, JsonLine command) {
                ParticipantId participantId = ParticipantId.of(command.required("participant_id"));
                ParticipantHandle handle = session.registerParticipant(
                        participantId, specification(command));
                if (participants.putIfAbsent(participantId, handle) != null) {
                    handle.unregister();
                    throw new IllegalArgumentException("participant is already registered locally");
                }
                installParticipantListener(handle);
                Optional<ConnectionCredential> credential = handle.connectionCredential();
                if (credential.isPresent()) {
                    emitCredential(handle, credential.get());
                }
                track(correlationId, participantId.value(), handle.whenOwned(), "owned");
            }

            private void setSpec(String correlationId, JsonLine command) {
                ParticipantHandle handle = participant(command);
                CompletableFuture<AcceptedRevision> future = handle.setSpec(specification(command));
                future.whenComplete((accepted, failure) -> {
                    operations.release();
                    if (failure != null) {
                        emit(error(correlationId, failure));
                        return;
                    }
                    Map<String, Object> event = event(
                            "accepted", correlationId, handle.participantId().value());
                    event.put("client_spec_revision", Long.toUnsignedString(
                            accepted.clientSpecRevision()));
                    event.put("accepted_spec_revision", Long.toUnsignedString(
                            accepted.acceptedSpecRevision()));
                    event.put("applied_spec_revision", Long.toUnsignedString(
                            accepted.appliedSpecRevision()));
                    event.put("published_generation", Long.toUnsignedString(
                            accepted.publishedGeneration()));
                    emit(event);
                });
            }

            private void release(String correlationId, JsonLine command) {
                ParticipantHandle handle = participant(command);
                participants.remove(handle.participantId(), handle);
                track(correlationId, handle.participantId().value(), handle.unregister(), "released");
            }

            private void fetchSpace(String correlationId, JsonLine command) {
                session.fetchSpace(SpaceKey.of(command.required("space_key")))
                        .whenComplete((snapshot, failure) -> {
                            operations.release();
                            if (failure != null) {
                                emit(error(correlationId, failure));
                                return;
                            }
                            emit(snapshotEvent("space_snapshot", correlationId, snapshot));
                        });
            }

            private ParticipantHandle participant(JsonLine command) {
                ParticipantId participantId = ParticipantId.of(command.required("participant_id"));
                ParticipantHandle handle = participants.get(participantId);
                if (handle == null) {
                    throw new IllegalArgumentException(
                            "unknown local participant: " + participantId);
                }
                return handle;
            }

            private void track(
                    String correlationId,
                    String participantId,
                    CompletableFuture<Void> operation,
                    String successKind) {
                operation.whenComplete((ignored, failure) -> {
                    operations.release();
                    if (failure == null) {
                        emit(event(successKind, correlationId, participantId));
                    } else {
                        emit(error(correlationId, failure));
                    }
                });
            }

            private void installListeners() {
                session.addSessionListener(new ControllerSessionListener() {
                    @Override
                    public void onStateChanged(
                            ControllerSession ignored,
                            ControllerSessionState previous,
                            ControllerSessionState current) {
                        Map<String, Object> event = event("session_state", "", "");
                        event.put("previous", previous.name());
                        event.put("current", current.name());
                        emit(event);
                    }
                });
                session.addSpaceListener(new SpaceListener() {
                    @Override
                    public void onSpaceUpdated(ControllerSession ignored, SpaceSnapshot snapshot) {
                        if (eventMode == DriverEventMode.FULL) {
                            emit(snapshotEvent("space_updated", "", snapshot));
                        } else {
                            output.suppress();
                        }
                    }
                });
            }

            private void installParticipantListener(ParticipantHandle handle) {
                handle.addListener(new ParticipantListener() {
                    @Override
                    public void onStateChanged(
                            ParticipantHandle participant,
                            ParticipantHandleState previous,
                            ParticipantHandleState current) {
                        Map<String, Object> event = event(
                                "ownership_state", "", participant.participantId().value());
                        event.put("previous", previous.name());
                        event.put("current", current.name());
                        emit(event);
                    }

                    @Override
                    public void onStatusChanged(
                            ParticipantHandle participant, ParticipantStatus status) {
                        Map<String, Object> event = event(
                                "participant_status", "", participant.participantId().value());
                        event.put("connected", status.connected());
                        event.put("space_key", status.appliedSpaceKey().isPresent()
                                ? status.appliedSpaceKey().get().value() : "");
                        event.put("self_mute", status.selfMute());
                        event.put("self_deaf", status.selfDeaf());
                        event.put("accepted_spec_revision", Long.toUnsignedString(
                                status.acceptedSpecRevision()));
                        event.put("applied_spec_revision", Long.toUnsignedString(
                                status.appliedSpecRevision()));
                        event.put("published_generation", Long.toUnsignedString(
                                status.publishedGeneration()));
                        event.put("application_error", status.applicationError().orElse(""));
                        if (eventMode == DriverEventMode.FULL) {
                            emit(event);
                        } else {
                            output.emitLatest(
                                    controllerId.value() + ":participant_status:"
                                            + participant.participantId().value(),
                                    event);
                        }
                    }

                    @Override
                    public void onConnectionCredentialChanged(
                            ParticipantHandle participant, ConnectionCredential credential) {
                        emitCredential(participant, credential);
                    }

                    @Override
                    public void onOwnershipLost(ParticipantHandle participant, String reason) {
                        participants.remove(participant.participantId(), participant);
                        Map<String, Object> event = event(
                                "ownership_lost", "", participant.participantId().value());
                        event.put("reason", reason);
                        emit(event);
                    }
                });
            }

            private void emitCredential(
                    ParticipantHandle participant, ConnectionCredential credential) {
                Map<String, Object> event = event(
                        "credential", "", participant.participantId().value());
                event.put("credential", credential.value());
                emit(event);
            }

            private Map<String, Object> snapshotEvent(
                    String kind, String correlationId, SpaceSnapshot snapshot) {
                Map<String, Object> event = event(kind, correlationId, "");
                event.put("space_key", snapshot.spaceKey().value());
                event.put("space_revision", Long.toUnsignedString(snapshot.spaceRevision()));
                event.put("published_generation", Long.toUnsignedString(
                        snapshot.publishedGeneration()));
                event.put("participant_count", snapshot.participants().size());
                return event;
            }

            private Map<String, Object> event(
                    String kind, String correlationId, String participantId) {
                return DriverHost.this.event(
                        kind, correlationId, controllerId.value(), participantId);
            }

            private Map<String, Object> error(String correlationId, Throwable failure) {
                return DriverHost.this.error(correlationId, controllerId.value(), failure);
            }

            private void emit(Map<String, Object> event) {
                DriverHost.this.emit(event);
            }
        }
    }

    private static ParticipantSpec specification(JsonLine command) {
        return ParticipantSpec.builder(
                        SpaceKey.of(command.required("space_key")),
                        command.required("display_name"))
                .serverMute(command.optionalBoolean("server_mute", false))
                .serverDeaf(command.optionalBoolean("server_deaf", false))
                .build();
    }

    private static String safeMessage(Throwable failure) {
        String message = failure.getMessage();
        return message == null ? failure.getClass().getSimpleName() : message;
    }
}
