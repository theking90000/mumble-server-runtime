package be.theking90000.mumble.controller;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import be.theking90000.mumble.controller.internal.core.v1.ClientFrame;
import be.theking90000.mumble.controller.internal.core.v1.DesiredStateReconciled;
import be.theking90000.mumble.controller.internal.core.v1.ParticipantOwnershipGranted;
import be.theking90000.mumble.controller.internal.core.v1.ParticipantStatus;
import be.theking90000.mumble.controller.internal.core.v1.ParticipantStatusChanged;
import be.theking90000.mumble.controller.internal.core.v1.ProfileEvent;
import be.theking90000.mumble.controller.internal.core.v1.ProfileRef;
import be.theking90000.mumble.controller.internal.core.v1.ServerFrame;
import be.theking90000.mumble.controller.internal.core.v1.SessionReady;
import com.google.protobuf.ByteString;
import com.google.protobuf.Duration;
import java.net.URI;
import java.util.Random;
import java.util.concurrent.CompletableFuture;
import org.junit.jupiter.api.Test;

final class CoreSessionTest {
    private static final ProfileReference PROFILE = ProfileReference.of(
            "example.profile", 3, repeat('a', 64));
    private static final ByteString SESSION_TOKEN = ByteString.copyFromUtf8("session-token");
    private static final ByteString RESUME_TOKEN = ByteString.copyFromUtf8("resume-token");

    @Test
    void openingSnapshotNegotiatesTheExactProfileAndCarriesOpaqueState() {
        Fixture fixture = new Fixture(ProfilePayload.of(new byte[] {1, 2, 3}));
        CoreParticipantHandle participant = fixture.session.registerParticipant(
                ParticipantId.of("player-1"), ProfilePayload.of(new byte[] {4, 5}));

        CompletableFuture<Void> started = fixture.session.start();
        ClientFrame open = fixture.transport().lastSent();

        assertEquals(ClientFrame.PayloadCase.OPEN_SESSION, open.getPayloadCase());
        assertEquals(PROFILE.profileId(), open.getOpenSession().getProfile().getProfileId());
        assertEquals(PROFILE.schemaVersion(), open.getOpenSession().getProfile().getSchemaVersion());
        assertEquals(
                PROFILE.descriptorDigest(),
                open.getOpenSession().getProfile().getDescriptorDigest());
        assertArrayEquals(
                new byte[] {1, 2, 3},
                open.getOpenSession().getDesiredState().getProfileState().getProtobuf().toByteArray());
        assertArrayEquals(
                new byte[] {4, 5},
                open.getOpenSession().getDesiredState().getParticipants(0)
                        .getProfileSpec().getProtobuf().toByteArray());
        assertFalse(started.isDone());

        fixture.ready(open.getRequestId());
        fixture.grant(participant);
        fixture.reconcile(open.getOpenSession().getDesiredState().getDesiredStateRevision());

        assertTrue(started.isDone());
        assertEquals(ControllerSessionState.ACTIVE, fixture.session.state());
        assertEquals(ParticipantHandleState.OWNED, participant.state());
        assertEquals("credential", participant.connectionCredential().get().value());
    }

    @Test
    void profileStateAndCommandsRemainOpaqueToCore() {
        Fixture fixture = new Fixture(ProfilePayload.empty());
        fixture.activate();

        CompletableFuture<Void> stateAccepted = fixture.session.setProfileState(
                ProfilePayload.of(new byte[] {9}));
        ClientFrame sync = fixture.transport().lastSent();
        assertEquals(ClientFrame.PayloadCase.SYNC_DESIRED_STATE, sync.getPayloadCase());
        assertArrayEquals(new byte[] {9}, sync.getSyncDesiredState().getDesiredState()
                .getProfileState().getProtobuf().toByteArray());
        fixture.reconcile(sync.getSyncDesiredState().getDesiredState().getDesiredStateRevision());
        assertTrue(stateAccepted.isDone());

        CompletableFuture<ProfilePayload> response = fixture.session.sendProfileCommand(
                ProfilePayload.of(new byte[] {10}));
        ClientFrame command = fixture.transport().lastSent();
        assertArrayEquals(new byte[] {10}, command.getProfileCommand()
                .getPayload().getProtobuf().toByteArray());
        fixture.transport().emit(ServerFrame.newBuilder()
                .setRequestId(command.getRequestId())
                .setProfileEvent(ProfileEvent.newBuilder()
                        .setPayload(wirePayload(new byte[] {11})))
                .build());
        assertArrayEquals(new byte[] {11}, response.join().protobuf());
    }

    @Test
    void genericParticipantStatusPreservesProfilePayload() {
        Fixture fixture = new Fixture(ProfilePayload.empty());
        CoreParticipantHandle participant = fixture.session.registerParticipant(
                ParticipantId.of("player-1"), ProfilePayload.empty());
        fixture.activate(participant);

        fixture.transport().emit(ServerFrame.newBuilder()
                .setParticipantStatusChanged(ParticipantStatusChanged.newBuilder()
                        .setParticipantId("player-1")
                        .setStatus(ParticipantStatus.newBuilder()
                                .setConnected(true)
                                .setAcceptedSpecRevision(4L)
                                .setAppliedSpecRevision(3L)
                                .setPublishedGeneration(2L)
                                .setProfileStatus(wirePayload(new byte[] {12}))))
                .build());

        CoreParticipantStatus status = participant.latestStatus().get();
        assertTrue(status.connected());
        assertEquals(4L, status.acceptedSpecRevision());
        assertArrayEquals(new byte[] {12}, status.profileStatus().protobuf());
    }

    @Test
    void reconnectResendsLatestStateAndKnownOwnership() {
        Fixture fixture = new Fixture(ProfilePayload.of(new byte[] {1}));
        CoreParticipantHandle participant = fixture.session.registerParticipant(
                ParticipantId.of("player-1"), ProfilePayload.of(new byte[] {2}));
        fixture.activate(participant);

        fixture.transport().disconnect(true);
        participant.setSpec(ProfilePayload.of(new byte[] {3}));
        fixture.session.setProfileState(ProfilePayload.of(new byte[] {4}));
        fixture.scheduler.runNext();

        ClientFrame reconnect = fixture.transport().lastSent();
        assertEquals(RESUME_TOKEN, reconnect.getOpenSession().getResumeToken());
        assertFalse(reconnect.getOpenSession().getDesiredState().getParticipants(0)
                .getOwnershipToken().isEmpty());
        assertArrayEquals(new byte[] {3}, reconnect.getOpenSession().getDesiredState()
                .getParticipants(0).getProfileSpec().getProtobuf().toByteArray());
        assertArrayEquals(new byte[] {4}, reconnect.getOpenSession().getDesiredState()
                .getProfileState().getProtobuf().toByteArray());
    }

    @Test
    void missingProfileEventPayloadFailsClosed() {
        Fixture fixture = new Fixture(ProfilePayload.empty());
        fixture.activate();

        fixture.transport().emit(ServerFrame.newBuilder()
                .setProfileEvent(ProfileEvent.getDefaultInstance())
                .build());

        assertEquals(ControllerSessionState.FAILED, fixture.session.state());
    }

    private static be.theking90000.mumble.controller.internal.core.v1.ProfilePayload
            wirePayload(byte[] value) {
        return be.theking90000.mumble.controller.internal.core.v1.ProfilePayload.newBuilder()
                .setProtobuf(ByteString.copyFrom(value))
                .build();
    }

    private static String repeat(char value, int count) {
        StringBuilder result = new StringBuilder(count);
        for (int index = 0; index < count; index++) {
            result.append(value);
        }
        return result.toString();
    }

    private static final class Fixture {
        private final ScriptedCoreTransport.Factory transports =
                new ScriptedCoreTransport.Factory();
        private final ManualCoreSessionScheduler scheduler =
                new ManualCoreSessionScheduler();
        private final CoreSession session;

        private Fixture(ProfilePayload initialState) {
            session = CoreSession.builder(
                            ControllerId.of("controller-1"),
                            URI.create("http://127.0.0.1:4000"),
                            PROFILE)
                    .initialProfileState(initialState)
                    .callbackExecutor(Runnable::run)
                    .transportFactory(transports)
                    .scheduler(scheduler)
                    .random(new Random(0L))
                    .build();
        }

        private ScriptedCoreTransport transport() {
            return transports.current();
        }

        private void activate(CoreParticipantHandle... participants) {
            session.start();
            ClientFrame open = transport().lastSent();
            ready(open.getRequestId());
            for (CoreParticipantHandle participant : participants) {
                grant(participant);
            }
            reconcile(open.getOpenSession().getDesiredState().getDesiredStateRevision());
        }

        private void ready(ByteString requestId) {
            transport().emit(ServerFrame.newBuilder()
                    .setRequestId(requestId)
                    .setSessionReady(SessionReady.newBuilder()
                            .setSessionToken(SESSION_TOKEN)
                            .setResumeToken(RESUME_TOKEN)
                            .setLeaseDuration(Duration.newBuilder().setSeconds(30L))
                            .setProfile(ProfileRef.newBuilder()
                                    .setProfileId(PROFILE.profileId())
                                    .setSchemaVersion(PROFILE.schemaVersion())
                                    .setDescriptorDigest(PROFILE.descriptorDigest())))
                    .build());
        }

        private void grant(CoreParticipantHandle participant) {
            transport().emit(ServerFrame.newBuilder()
                    .setParticipantOwnershipGranted(ParticipantOwnershipGranted.newBuilder()
                            .setParticipantId(participant.participantId().value())
                            .setRegistrationId(participant.registrationId())
                            .setOwnershipToken(ByteString.copyFromUtf8("ownership"))
                            .setConnectionCredential("credential")
                            .setClientSpecRevision(participant.clientSpecRevision())
                            .setAcceptedSpecRevision(1L)
                            .setAppliedSpecRevision(1L)
                            .setPublishedGeneration(1L))
                    .build());
        }

        private void reconcile(long revision) {
            transport().emit(ServerFrame.newBuilder()
                    .setDesiredStateReconciled(DesiredStateReconciled.newBuilder()
                            .setDesiredStateRevision(revision))
                    .build());
        }
    }
}
