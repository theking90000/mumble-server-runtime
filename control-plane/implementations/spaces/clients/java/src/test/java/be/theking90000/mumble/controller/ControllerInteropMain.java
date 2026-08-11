package be.theking90000.mumble.controller;

import java.io.BufferedReader;
import java.io.BufferedWriter;
import java.io.InputStreamReader;
import java.net.URI;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.nio.file.StandardOpenOption;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;

/** Process-side Java SDK driver used only by the Rust-Java-Mumble interop test. */
public final class ControllerInteropMain {
    private static final long TIMEOUT_SECONDS = 20L;

    private ControllerInteropMain() {
    }

    /** Runs the pipe-controlled interop scenario without writing credentials to stdout. */
    public static void main(String[] arguments) throws Exception {
        if (arguments.length != 2) {
            throw new IllegalArgumentException("expected endpoint and token file");
        }
        URI endpoint = URI.create(arguments[0]);
        Path tokenFile = Paths.get(arguments[1]);
        ControllerSession primary = ControllerSession.builder(
                        ControllerId.of("controller-interop-primary"), endpoint)
                .build();
        ParticipantHandle alice = primary.registerParticipant(
                ParticipantId.of("alice"), spec("lobby", "Alice", false));
        ParticipantHandle bob = primary.registerParticipant(
                ParticipantId.of("bob"), spec("lobby", "Bob", false));
        primary.start().get(TIMEOUT_SECONDS, TimeUnit.SECONDS);
        String aliceToken = alice.whenMumbleJoinTokenAvailable()
                .get(TIMEOUT_SECONDS, TimeUnit.SECONDS)
                .value();
        String bobToken = bob.whenMumbleJoinTokenAvailable()
                .get(TIMEOUT_SECONDS, TimeUnit.SECONDS)
                .value();
        try (BufferedWriter tokens = Files.newBufferedWriter(
                tokenFile,
                StandardCharsets.UTF_8,
                StandardOpenOption.TRUNCATE_EXISTING,
                StandardOpenOption.WRITE)) {
            tokens.write(aliceToken);
            tokens.newLine();
            tokens.write(bobToken);
            tokens.newLine();
        }
        aliceToken = null;
        bobToken = null;

        ControllerSession transfer = null;
        BufferedReader commands = new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8));
        System.out.println("CONTROLLER_INTEROP_READY");
        System.out.flush();
        String command;
        while ((command = commands.readLine()) != null) {
            if ("MOVE".equals(command)) {
                AcceptedRevision accepted = alice.setSpec(spec("arena", "Alice", false))
                        .get(TIMEOUT_SECONDS, TimeUnit.SECONDS);
                awaitApplied(alice, accepted.acceptedSpecRevision(), "arena");
                SpaceSnapshot snapshot = primary.fetchSpace(SpaceKey.of("arena"))
                        .get(TIMEOUT_SECONDS, TimeUnit.SECONDS);
                if (snapshot.publishedGeneration() == 0L || snapshot.participants().isEmpty()) {
                    throw new IllegalStateException("moved Space was not published");
                }
                acknowledge("MOVED");
            } else if ("MUTE".equals(command)) {
                AcceptedRevision accepted = alice.setSpec(spec(
                                alice.desiredSpec().spaceKey().value(), "Alice", true))
                        .get(TIMEOUT_SECONDS, TimeUnit.SECONDS);
                awaitApplied(
                        alice,
                        accepted.acceptedSpecRevision(),
                        alice.desiredSpec().spaceKey().value());
                acknowledge("MUTED");
            } else if ("UNMUTE".equals(command)) {
                AcceptedRevision accepted = alice.setSpec(spec(
                                alice.desiredSpec().spaceKey().value(), "Alice", false))
                        .get(TIMEOUT_SECONDS, TimeUnit.SECONDS);
                awaitApplied(
                        alice,
                        accepted.acceptedSpecRevision(),
                        alice.desiredSpec().spaceKey().value());
                acknowledge("UNMUTED");
            } else if ("DEAF_BOB".equals(command)) {
                ParticipantSpec deaf = ParticipantSpec.builder(SpaceKey.of("lobby"), "Bob")
                        .serverDeaf(true)
                        .build();
                AcceptedRevision accepted = bob.setSpec(deaf)
                        .get(TIMEOUT_SECONDS, TimeUnit.SECONDS);
                awaitApplied(bob, accepted.acceptedSpecRevision(), "lobby");
                acknowledge("BOB_DEAF");
            } else if ("UNDEAF_BOB".equals(command)) {
                AcceptedRevision accepted = bob.setSpec(spec("lobby", "Bob", false))
                        .get(TIMEOUT_SECONDS, TimeUnit.SECONDS);
                awaitApplied(bob, accepted.acceptedSpecRevision(), "lobby");
                acknowledge("BOB_UNDEAF");
            } else if ("WAIT_SELF_MUTED".equals(command)) {
                awaitSelfMute(alice);
                acknowledge("SELF_MUTED");
            } else if ("TRANSFER".equals(command)) {
                transfer = ControllerSession.builder(
                                ControllerId.of("controller-interop-transfer"), endpoint)
                        .build();
                transfer.start().get(TIMEOUT_SECONDS, TimeUnit.SECONDS);
                alice = transfer.registerParticipant(
                        ParticipantId.of("alice"), spec("handoff", "Alice transferred", false));
                alice.whenOwned().get(TIMEOUT_SECONDS, TimeUnit.SECONDS);
                alice.whenMumbleJoinTokenAvailable().get(TIMEOUT_SECONDS, TimeUnit.SECONDS);
                awaitApplied(alice, alice.latestStatus()
                        .map(ParticipantStatus::acceptedSpecRevision)
                        .orElse(1L), "handoff");
                acknowledge("TRANSFERRED");
            } else if ("RELEASE".equals(command)) {
                alice.unregister().get(TIMEOUT_SECONDS, TimeUnit.SECONDS);
                acknowledge("RELEASED");
            } else if ("STOP".equals(command)) {
                if (transfer != null) {
                    transfer.stop().get(TIMEOUT_SECONDS, TimeUnit.SECONDS);
                }
                primary.stop().get(TIMEOUT_SECONDS, TimeUnit.SECONDS);
                acknowledge("STOPPED");
                return;
            } else {
                throw new IllegalArgumentException("unknown interop command");
            }
        }
    }

    private static ParticipantSpec spec(String space, String name, boolean serverMute) {
        return ParticipantSpec.builder(SpaceKey.of(space), name)
                .serverMute(serverMute)
                .build();
    }

    private static void awaitApplied(
            ParticipantHandle participant,
            long acceptedRevision,
            String expectedSpace) throws Exception {
        CompletableFuture<Void> applied = new CompletableFuture<Void>();
        ParticipantListener listener = new ParticipantListener() {
            @Override
            public void onStatusChanged(ParticipantHandle ignored, ParticipantStatus status) {
                if (Long.compareUnsigned(status.appliedSpecRevision(), acceptedRevision) >= 0
                        && status.appliedSpaceKey().isPresent()
                        && expectedSpace.equals(status.appliedSpaceKey().get().value())
                        && status.publishedGeneration() > 0L
                        && !status.applicationError().isPresent()) {
                    applied.complete(null);
                }
            }
        };
        participant.addListener(listener);
        try {
            if (participant.latestStatus().isPresent()) {
                listener.onStatusChanged(participant, participant.latestStatus().get());
            }
            applied.get(TIMEOUT_SECONDS, TimeUnit.SECONDS);
        } finally {
            participant.removeListener(listener);
        }
    }

    private static void awaitSelfMute(ParticipantHandle participant) throws Exception {
        CompletableFuture<Void> muted = new CompletableFuture<Void>();
        ParticipantListener listener = new ParticipantListener() {
            @Override
            public void onStatusChanged(ParticipantHandle ignored, ParticipantStatus status) {
                if (status.selfMute()) {
                    muted.complete(null);
                }
            }
        };
        participant.addListener(listener);
        try {
            if (participant.latestStatus().isPresent()) {
                listener.onStatusChanged(participant, participant.latestStatus().get());
            }
            muted.get(TIMEOUT_SECONDS, TimeUnit.SECONDS);
        } finally {
            participant.removeListener(listener);
        }
    }

    private static void acknowledge(String value) {
        System.out.println("CONTROLLER_INTEROP_" + value);
        System.out.flush();
    }
}
