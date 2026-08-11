package be.theking90000.mumble.controller.spaces;

import be.theking90000.mumble.controller.core.*;
import be.theking90000.mumble.controller.core.CoreSessionTestkit;

import com.google.protobuf.ByteString;
import com.google.protobuf.Duration;
import be.theking90000.mumble.controller.spaces.internal.ProfileMetadata;
import be.theking90000.mumble.controller.internal.core.v1.ClientFrame;
import be.theking90000.mumble.controller.internal.core.v1.CommandErrorCode;
import be.theking90000.mumble.controller.internal.core.v1.CommandRejected;
import be.theking90000.mumble.controller.internal.core.v1.DesiredStateReconciled;
import be.theking90000.mumble.controller.internal.core.v1.OwnershipRevocationReason;
import be.theking90000.mumble.controller.internal.core.v1.ParticipantOwnershipGranted;
import be.theking90000.mumble.controller.internal.core.v1.ParticipantOwnershipRevoked;
import be.theking90000.mumble.controller.internal.core.v1.ProfileEvent;
import be.theking90000.mumble.controller.internal.core.v1.ProfileRef;
import be.theking90000.mumble.controller.internal.core.v1.ServerFrame;
import be.theking90000.mumble.controller.internal.core.v1.SessionClosing;
import be.theking90000.mumble.controller.internal.core.v1.SessionReady;
import be.theking90000.mumble.controller.internal.spaces.v1.Event;
import be.theking90000.mumble.controller.internal.spaces.v1.FetchSpaceResult;
import be.theking90000.mumble.controller.internal.spaces.v1.ObservedSpacesAccepted;
import java.net.URI;
import java.util.Arrays;
import java.util.Random;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CompletionException;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

final class ControllerSessionTest {
    private static final ByteString SESSION_TOKEN = ByteString.copyFromUtf8("session-token");
    private static final ByteString RESUME_TOKEN = ByteString.copyFromUtf8("resume-token");
    private static final ByteString CONTROL_EPOCH = ByteString.copyFromUtf8("control-epoch");
    private static final ByteString OWNERSHIP_TOKEN = ByteString.copyFromUtf8("ownership-token");
    private static final String CONNECTION_CREDENTIAL = "mumble-join-token";

    @Test
    void initialSnapshotBecomesActiveOnlyAfterReadyAndReconciliation() {
        Fixture fixture = new Fixture();
        ParticipantHandle participant = fixture.session.registerParticipant(
                ParticipantId.of("player-1"),
                spec("lobby", "Player One"));
        CompletableFuture<Void> observed = fixture.session.observeSpace(SpaceKey.of("staff"));
        assertSame(observed, fixture.session.observeSpace(SpaceKey.of("staff")));

        CompletableFuture<Void> started = fixture.session.start();
        ClientFrame open = fixture.transport().lastSent();

        assertEquals(ClientFrame.PayloadCase.OPEN_SESSION, open.getPayloadCase());
        assertEquals("lobby-11", open.getOpenSession().getControllerId());
        assertTrue(open.getOpenSession().hasProfile());
        assertEquals(
                ProfileMetadata.SPACES_PROFILE_ID,
                open.getOpenSession().getProfile().getProfileId());
        assertEquals(
                ProfileMetadata.SPACES_SCHEMA_VERSION,
                open.getOpenSession().getProfile().getSchemaVersion());
        assertEquals(
                ProfileMetadata.SPACES_DESCRIPTOR_DIGEST,
                open.getOpenSession().getProfile().getDescriptorDigest());
        assertEquals(1, open.getOpenSession().getDesiredState().getParticipantsCount());
        assertEquals(Arrays.asList("staff"), SpacesWireTestkit.desiredState(
                open.getOpenSession().getDesiredState().getProfileState())
                .getObservedSpaceKeysList());
        assertFalse(started.isDone());

        fixture.emitReady(open.getRequestId());
        assertEquals(ControllerSessionState.RECONCILING, fixture.session.state());
        assertFalse(started.isDone());

        fixture.grant(participant, 1L);
        fixture.transport().emit(profileEvent(Event.newBuilder()
                .setObservedSpacesAccepted(ObservedSpacesAccepted.newBuilder()
                        .setObservedSpacesRevision(1L))
                .build()));
        fixture.reconcile(open.getOpenSession().getDesiredState().getDesiredStateRevision());

        assertEquals(ControllerSessionState.ACTIVE, fixture.session.state());
        assertTrue(started.isDone());
        assertTrue(observed.isDone());
        assertEquals(ParticipantHandleState.OWNED, participant.state());
        assertTrue(participant.whenOwned().isDone());
        assertEquals(CONNECTION_CREDENTIAL, participant.whenConnectionCredentialAvailable().join().value());
        assertEquals(CONNECTION_CREDENTIAL, participant.connectionCredential().get().value());
        assertFalse(participant.connectionCredential().get().toString().contains(CONNECTION_CREDENTIAL));
    }

    @Test
    void reconnectCoalescesOfflineSpecsAndPresentsKnownOwnership() {
        Fixture fixture = new Fixture();
        ParticipantHandle participant = fixture.session.registerParticipant(
                ParticipantId.of("player-1"), spec("lobby", "One"));
        ClientFrame firstOpen = fixture.startAndActivate(participant);

        fixture.transport().disconnect(true);
        assertEquals(ParticipantHandleState.SUSPENDED, participant.state());
        assertEquals(ControllerSessionState.RECONNECTING, fixture.session.state());
        assertTrue(fixture.scheduler.nextDelayMillis() >= 200L);
        assertTrue(fixture.scheduler.nextDelayMillis() <= 300L);

        CompletableFuture<AcceptedRevision> first = participant.setSpec(spec("game", "Two"));
        CompletableFuture<AcceptedRevision> second = participant.setSpec(spec("game", "Three"));
        fixture.scheduler.runNext();

        assertEquals(2, fixture.transports.createdCount());
        ClientFrame reconnect = fixture.transport().lastSent();
        assertEquals(ClientFrame.PayloadCase.OPEN_SESSION, reconnect.getPayloadCase());
        assertEquals(RESUME_TOKEN, reconnect.getOpenSession().getResumeToken());
        assertEquals(OWNERSHIP_TOKEN,
                reconnect.getOpenSession().getDesiredState().getParticipants(0).getOwnershipToken());
        assertEquals("Three", SpacesWireTestkit.participantSpec(reconnect.getOpenSession()
                .getDesiredState()
                .getParticipants(0)
                .getProfileSpec()).getDisplayName());
        assertEquals(3L,
                reconnect.getOpenSession().getDesiredState().getParticipants(0).getClientSpecRevision());

        fixture.emitReady(reconnect.getRequestId());
        fixture.grant(participant, 3L);
        fixture.reconcile(reconnect.getOpenSession().getDesiredState().getDesiredStateRevision());

        assertEquals(ControllerSessionState.ACTIVE, fixture.session.state());
        assertTrue(first.isDone());
        assertTrue(second.isDone());
        assertEquals(3L, first.join().clientSpecRevision());
        assertEquals(10L, first.join().acceptedSpecRevision());
        assertEquals(9L, first.join().appliedSpecRevision());
        assertEquals(8L, first.join().publishedGeneration());
        assertEquals(firstOpen.getOpenSession().getControllerInstanceId(),
                reconnect.getOpenSession().getControllerInstanceId());
        assertEquals(CONNECTION_CREDENTIAL, participant.connectionCredential().get().value());
    }

    @Test
    void aReacquisitionRotatesTheConnectionCredentialAndNotifiesListeners() {
        Fixture fixture = new Fixture();
        ParticipantHandle participant = fixture.session.registerParticipant(
                ParticipantId.of("player-1"), spec("lobby", "One"));
        final java.util.List<String> observed = new java.util.ArrayList<String>();
        participant.addListener(new ParticipantListener() {
            @Override
            public void onConnectionCredentialChanged(ParticipantHandle ignored, ConnectionCredential token) {
                observed.add(token.value());
            }
        });
        fixture.startAndActivate(participant);

        fixture.grant(
                participant,
                CoreSessionTestkit.clientSpecRevision(participant.coreHandle()),
                "rotated-token");

        assertEquals(Arrays.asList(CONNECTION_CREDENTIAL, "rotated-token"), observed);
        assertEquals("rotated-token", participant.connectionCredential().get().value());
        assertEquals(CONNECTION_CREDENTIAL, participant.whenConnectionCredentialAvailable().join().value());
    }

    @Test
    void ownershipRejectionRevokesAndRemovesTheHandle() {
        Fixture fixture = new Fixture();
        ParticipantHandle participant = fixture.session.registerParticipant(
                ParticipantId.of("player-1"), spec("lobby", "One"));
        fixture.startAndActivate(participant);

        CompletableFuture<AcceptedRevision> update = participant.setSpec(spec("game", "One"));
        ClientFrame setSpec = fixture.transport().lastSent();
        fixture.transport().emit(ServerFrame.newBuilder()
                .setRequestId(setSpec.getRequestId())
                .setCommandRejected(CommandRejected.newBuilder()
                        .setCode(CommandErrorCode.OWNERSHIP_LOST)
                        .setMessage("replaced by another controller")
                        .build())
                .build());

        assertEquals(ParticipantHandleState.REVOKED, participant.state());
        assertFalse(participant.connectionCredential().isPresent());
        assertFalse(fixture.session.participant(participant.participantId()).isPresent());
        CompletionException failure = assertThrows(CompletionException.class, update::join);
        assertTrue(failure.getCause() instanceof OwnershipLostException);
        assertThrows(SessionClosedException.class, () -> participant.setSpec(spec("game", "Late")));
    }

    @Test
    void observeAndUnobserveAlwaysSendTheCompleteExplicitSet() {
        Fixture fixture = new Fixture();
        fixture.startAndActivate(null);

        fixture.session.observeSpace(SpaceKey.of("alpha"));
        ClientFrame alpha = fixture.transport().lastSent();
        assertEquals(Arrays.asList("alpha"), SpacesWireTestkit.decodedCommand(alpha.getProfileCommand())
                .getReplaceObservedSpaces().getSpaceKeysList());

        fixture.session.observeSpace(SpaceKey.of("beta"));
        ClientFrame alphaBeta = fixture.transport().lastSent();
        assertEquals(Arrays.asList("alpha", "beta"), SpacesWireTestkit.decodedCommand(
                alphaBeta.getProfileCommand()).getReplaceObservedSpaces().getSpaceKeysList());

        fixture.session.unobserveSpace(SpaceKey.of("alpha"));
        ClientFrame beta = fixture.transport().lastSent();
        assertEquals(Arrays.asList("beta"), SpacesWireTestkit.decodedCommand(beta.getProfileCommand())
                .getReplaceObservedSpaces().getSpaceKeysList());
    }

    @Test
    void fetchIsOneShotWhileStreamSnapshotsUseIncarnationAndRevision() {
        Fixture fixture = new Fixture();
        fixture.startAndActivate(null);

        CompletableFuture<SpaceSnapshot> fetched = fixture.session.fetchSpace(SpaceKey.of("game"));
        ClientFrame request = fixture.transport().lastSent();
        be.theking90000.mumble.controller.internal.spaces.v1.SpaceSnapshot first =
                wireSpace("game", "incarnation-a", 2L, "Player");
        fixture.transport().emit(profileEvent(
                request.getRequestId(),
                Event.newBuilder()
                        .setFetchSpaceResult(FetchSpaceResult.newBuilder().setSnapshot(first))
                        .build()));

        assertEquals(2L, fetched.join().spaceRevision());
        assertTrue(fixture.session.spaces().isEmpty());

        fixture.transport().emit(profileEvent(Event.newBuilder().setSpaceSnapshot(first).build()));
        fixture.transport().emit(profileEvent(Event.newBuilder()
                .setSpaceSnapshot(wireSpace("game", "incarnation-a", 1L, "Stale"))
                .build()));
        assertEquals("Player", fixture.session.spaces().get(SpaceKey.of("game"))
                .participants().get(0).displayName());

        fixture.transport().emit(profileEvent(Event.newBuilder()
                .setSpaceSnapshot(wireSpace("game", "incarnation-b", 1L, "New"))
                .build()));
        assertEquals("New", fixture.session.spaces().get(SpaceKey.of("game"))
                .participants().get(0).displayName());
    }

    @Test
    void staleRevocationCannotDetachAnotherRegistration() {
        Fixture fixture = new Fixture();
        ParticipantHandle participant = fixture.session.registerParticipant(
                ParticipantId.of("player-1"), spec("lobby", "One"));
        fixture.startAndActivate(participant);

        fixture.transport().emit(ServerFrame.newBuilder()
                .setParticipantOwnershipRevoked(ParticipantOwnershipRevoked.newBuilder()
                        .setParticipantId("player-1")
                        .setRegistrationId(ByteString.copyFromUtf8("stale-registration"))
                        .setReason(OwnershipRevocationReason.PARTICIPANT_RELEASED)
                        .build())
                .build());

        assertEquals(ParticipantHandleState.OWNED, participant.state());
        assertTrue(fixture.session.participant(participant.participantId()).isPresent());
    }

    @Test
    void localMutationDuringReconciliationForcesANewerFullSnapshot() {
        Fixture fixture = new Fixture();
        fixture.session.start();
        ClientFrame open = fixture.transport().lastSent();
        fixture.emitReady(open.getRequestId());

        fixture.session.observeSpace(SpaceKey.of("late"));
        fixture.reconcile(open.getOpenSession().getDesiredState().getDesiredStateRevision());

        ClientFrame sync = fixture.transport().lastSent();
        assertEquals(ClientFrame.PayloadCase.SYNC_DESIRED_STATE, sync.getPayloadCase());
        assertEquals(Arrays.asList("late"), SpacesWireTestkit.desiredState(sync.getSyncDesiredState()
                .getDesiredState()
                .getProfileState()).getObservedSpaceKeysList());
        assertEquals(ControllerSessionState.RECONCILING, fixture.session.state());
    }

    @Test
    void businessLeaseRenewsAtTwoThirdsWithoutUsingTransportKeepalive() {
        Fixture fixture = new Fixture();
        fixture.startAndActivate(null);

        assertEquals(20_000L, fixture.scheduler.nextDelayMillis());
        fixture.scheduler.runNext();

        assertEquals(ClientFrame.PayloadCase.RENEW_LEASE, fixture.transport().lastSent().getPayloadCase());
        assertEquals(SESSION_TOKEN, fixture.transport().lastSent().getRenewLease().getSessionToken());
        assertEquals(20_000L, fixture.scheduler.nextDelayMillis());
    }

    @Test
    void unregisterUsesTheExactGrantedCapability() {
        Fixture fixture = new Fixture();
        ParticipantHandle participant = fixture.session.registerParticipant(
                ParticipantId.of("player-1"), spec("lobby", "One"));
        fixture.startAndActivate(participant);

        CompletableFuture<Void> unregister = participant.unregister();
        ClientFrame release = fixture.transport().lastSent();

        assertEquals(ClientFrame.PayloadCase.RELEASE_PARTICIPANT, release.getPayloadCase());
        assertEquals(OWNERSHIP_TOKEN, release.getReleaseParticipant().getOwnershipToken());
        assertEquals(
                CoreSessionTestkit.registrationId(participant.coreHandle()),
                release.getReleaseParticipant().getRegistrationId());
        assertFalse(unregister.isDone());
        assertThrows(IllegalStateException.class, () -> fixture.session.registerParticipant(
                participant.participantId(), spec("lobby", "Replacement")));

        fixture.transport().emit(ServerFrame.newBuilder()
                .setRequestId(release.getRequestId())
                .setParticipantOwnershipRevoked(ParticipantOwnershipRevoked.newBuilder()
                        .setParticipantId("player-1")
                        .setRegistrationId(CoreSessionTestkit.registrationId(participant.coreHandle()))
                        .setReason(OwnershipRevocationReason.PARTICIPANT_RELEASED)
                        .build())
                .build());
        assertTrue(unregister.isDone());
        assertFalse(fixture.session.participant(participant.participantId()).isPresent());
        ParticipantHandle replacement = fixture.session.registerParticipant(
                participant.participantId(), spec("lobby", "Replacement"));
        assertEquals(ParticipantHandleState.ACQUIRING, replacement.state());
    }

    @Test
    void stopIsIdempotentAndClosesOnServerBarrier() {
        Fixture fixture = new Fixture();
        fixture.startAndActivate(null);

        CompletableFuture<Void> first = fixture.session.stop();
        CompletableFuture<Void> second = fixture.session.stop();
        assertSame(first, second);
        assertEquals(ControllerSessionState.STOPPING, fixture.session.state());

        ClientFrame close = fixture.transport().lastSent();
        fixture.transport().emit(ServerFrame.newBuilder()
                .setRequestId(close.getRequestId())
                .setSessionClosing(SessionClosing.newBuilder().setReason("closed").build())
                .build());

        assertTrue(first.isDone());
        assertEquals(ControllerSessionState.CLOSED, fixture.session.state());
        assertTrue(fixture.scheduler.isClosed());
    }

    @Test
    void observationDeclaredBeforeStartCompletesOnTheReconciliationBarrier() {
        Fixture fixture = new Fixture();
        CompletableFuture<Void> observed = fixture.session.observeSpace(SpaceKey.of("staff"));

        fixture.session.start();
        ClientFrame open = fixture.transport().lastSent();
        assertEquals(Arrays.asList("staff"), SpacesWireTestkit.desiredState(open.getOpenSession()
                .getDesiredState()
                .getProfileState()).getObservedSpaceKeysList());

        fixture.emitReady(open.getRequestId());
        fixture.reconcile(open.getOpenSession().getDesiredState().getDesiredStateRevision());

        assertEquals(ControllerSessionState.ACTIVE, fixture.session.state());
        assertTrue(observed.isDone());
    }

    @Test
    void observationPendingAcrossAReconnectCompletesOnTheNewBarrier() {
        Fixture fixture = new Fixture();
        fixture.startAndActivate(null);
        fixture.transport().disconnect(true);

        CompletableFuture<Void> observed = fixture.session.observeSpace(SpaceKey.of("staff"));
        assertFalse(observed.isDone());

        fixture.scheduler.runNext();
        ClientFrame reopen = fixture.transport().lastSent();
        assertEquals(Arrays.asList("staff"), SpacesWireTestkit.desiredState(reopen.getOpenSession()
                .getDesiredState()
                .getProfileState()).getObservedSpaceKeysList());
        fixture.emitReady(reopen.getRequestId());
        fixture.reconcile(reopen.getOpenSession().getDesiredState().getDesiredStateRevision());

        assertTrue(observed.isDone());
    }

    @Test
    void disconnectFailsFetchesThatTheNewStreamCannotAnswer() {
        Fixture fixture = new Fixture();
        fixture.startAndActivate(null);

        CompletableFuture<SpaceSnapshot> fetched = fixture.session.fetchSpace(SpaceKey.of("game"));
        assertFalse(fetched.isDone());

        fixture.transport().disconnect(true);

        assertTrue(fetched.isCompletedExceptionally());
        assertThrows(CompletionException.class, fetched::join);
    }

    @Test
    void disconnectDropsTheSpaceCacheItCanNoLongerVerify() {
        Fixture fixture = new Fixture();
        fixture.startAndActivate(null);
        fixture.transport().emit(profileEvent(Event.newBuilder()
                .setSpaceSnapshot(wireSpace("game", "incarnation-a", 2L, "Player"))
                .build()));
        assertFalse(fixture.session.spaces().isEmpty());

        fixture.transport().disconnect(true);

        assertTrue(fixture.session.spaces().isEmpty());
    }

    @Test
    void aMissingSpacesEventPayloadFailsTheSessionPermanently() {
        Fixture fixture = new Fixture();
        fixture.startAndActivate(null);

        fixture.transport().emit(ServerFrame.newBuilder()
                .setProfileEvent(ProfileEvent.getDefaultInstance())
                .build());

        assertEquals(ControllerSessionState.FAILED, fixture.session.state());
    }

    @Test
    void anExpiredLeaseRenewalReconnectsInsteadOfFailingTheSession() {
        Fixture fixture = new Fixture();
        fixture.startAndActivate(null);
        fixture.scheduler.runNext();

        ClientFrame renew = fixture.transport().lastSent();
        assertEquals(ClientFrame.PayloadCase.RENEW_LEASE, renew.getPayloadCase());
        fixture.transport().emit(ServerFrame.newBuilder()
                .setRequestId(renew.getRequestId())
                .setCommandRejected(CommandRejected.newBuilder()
                        .setCode(CommandErrorCode.SESSION_EXPIRED_ERROR)
                        .setMessage("lease deadline elapsed")
                        .build())
                .build());

        assertEquals(ControllerSessionState.RECONNECTING, fixture.session.state());
    }

    @Test
    void staleOwnershipGrantCannotDetachAnotherRegistration() {
        Fixture fixture = new Fixture();
        ParticipantHandle participant = fixture.session.registerParticipant(
                ParticipantId.of("player-1"), spec("lobby", "One"));
        fixture.startAndActivate(participant);

        fixture.transport().emit(ServerFrame.newBuilder()
                .setParticipantOwnershipGranted(ParticipantOwnershipGranted.newBuilder()
                        .setParticipantId("player-1")
                        .setRegistrationId(ByteString.copyFromUtf8("stale-registration"))
                        .setOwnershipToken(ByteString.copyFromUtf8("stale-token"))
                        .build())
                .build());

        assertEquals(ControllerSessionState.ACTIVE, fixture.session.state());
        assertEquals(ParticipantHandleState.OWNED, participant.state());
        assertTrue(fixture.session.participant(participant.participantId()).isPresent());
    }

    @Test
    void stopAfterAPermanentFailureStillCompletesNormally() {
        Fixture fixture = new Fixture();
        fixture.startAndActivate(null);

        fixture.transport().disconnect(false);
        assertEquals(ControllerSessionState.FAILED, fixture.session.state());

        fixture.session.stop().join();
        assertEquals(ControllerSessionState.CLOSED, fixture.session.state());
    }

    @Test
    void aSupersededTransportCannotResurrectTheSession() {
        Fixture fixture = new Fixture();
        fixture.startAndActivate(null);
        CoreSessionTestkit.Transport first = fixture.transport();

        first.disconnect(true);
        assertEquals(ControllerSessionState.RECONNECTING, fixture.session.state());

        first.signalConnected();

        assertEquals(ControllerSessionState.RECONNECTING, fixture.session.state());
        assertEquals(1, fixture.transports.createdCount());
    }

    @Test
    void endpointSchemeMustMatchTlsChoice() {
        assertThrows(IllegalArgumentException.class, () -> ControllerSession.builder(
                ControllerId.of("controller"), URI.create("https://localhost:4000")).build());
        assertThrows(IllegalArgumentException.class, () -> ControllerSession.builder(
                ControllerId.of("controller"), URI.create("http://localhost:4000"))
                .tls(TlsConfig.systemTrust())
                .build());
    }

    private static ParticipantSpec spec(String space, String name) {
        return ParticipantSpec.builder(SpaceKey.of(space), name).build();
    }

    private static ServerFrame profileEvent(Event event) {
        return profileEvent(ByteString.EMPTY, event);
    }

    private static ServerFrame profileEvent(ByteString requestId, Event event) {
        return ServerFrame.newBuilder()
                .setRequestId(requestId)
                .setProfileEvent(SpacesWireTestkit.profileEvent(event))
                .build();
    }

    private static be.theking90000.mumble.controller.internal.spaces.v1.SpaceSnapshot
            wireSpace(String key, String incarnation, long revision, String participantName) {
        return be.theking90000.mumble.controller.internal.spaces.v1.SpaceSnapshot
                .newBuilder()
                .setSpaceKey(key)
                .setIncarnationId(ByteString.copyFromUtf8(incarnation))
                .setSpaceRevision(revision)
                .addParticipants(be.theking90000.mumble.controller.internal.spaces.v1
                        .SpaceParticipant.newBuilder()
                        .setParticipantId("player-1")
                        .setDisplayName(participantName)
                        .setConnected(true)
                        .build())
                .setPublishedGeneration(7L)
                .build();
    }

    private static final class Fixture {
        private final CoreSessionTestkit.Factory transports =
                new CoreSessionTestkit.Factory();
        private final CoreSessionTestkit.Scheduler scheduler =
                new CoreSessionTestkit.Scheduler();
        private final CoreSession core = CoreSessionTestkit.session(
                ControllerId.of("lobby-11"),
                URI.create("http://127.0.0.1:4000"),
                ProfileReference.of(
                        ProfileMetadata.SPACES_PROFILE_ID,
                        ProfileMetadata.SPACES_SCHEMA_VERSION,
                        ProfileMetadata.SPACES_DESCRIPTOR_DIGEST),
                transports,
                scheduler);
        private final ControllerSession session = ControllerSession.builder(
                        ControllerId.of("lobby-11"),
                        URI.create("http://127.0.0.1:4000"))
                .callbackExecutor(Runnable::run)
                .coreSession(core)
                .build();

        private CoreSessionTestkit.Transport transport() {
            return transports.current();
        }

        private ClientFrame startAndActivate(ParticipantHandle participant) {
            session.start();
            ClientFrame open = transport().lastSent();
            emitReady(open.getRequestId());
            if (participant != null) {
                grant(
                        participant,
                        CoreSessionTestkit.clientSpecRevision(participant.coreHandle()));
            }
            reconcile(open.getOpenSession().getDesiredState().getDesiredStateRevision());
            return open;
        }

        private void emitReady(ByteString requestId) {
            transport().emit(ServerFrame.newBuilder()
                    .setRequestId(requestId)
                    .setSessionReady(SessionReady.newBuilder()
                            .setSessionToken(SESSION_TOKEN)
                            .setResumeToken(RESUME_TOKEN)
                            .setControlEpoch(CONTROL_EPOCH)
                            .setLeaseDuration(Duration.newBuilder().setSeconds(30L).build())
                            .setProfile(spacesProfile())
                            .build())
                    .build());
        }

        private void reconcile(long desiredRevision) {
            transport().emit(ServerFrame.newBuilder()
                    .setDesiredStateReconciled(DesiredStateReconciled.newBuilder()
                            .setDesiredStateRevision(desiredRevision)
                            .build())
                    .build());
        }

        private void grant(ParticipantHandle participant, long clientRevision) {
            grant(participant, clientRevision, CONNECTION_CREDENTIAL);
        }

        private void grant(ParticipantHandle participant, long clientRevision, String joinToken) {
            transport().emit(ServerFrame.newBuilder()
                    .setParticipantOwnershipGranted(ParticipantOwnershipGranted.newBuilder()
                            .setParticipantId(participant.participantId().value())
                            .setRegistrationId(CoreSessionTestkit.registrationId(
                                    participant.coreHandle()))
                            .setOwnershipToken(OWNERSHIP_TOKEN)
                            .setConnectionCredential(joinToken)
                            .setClientSpecRevision(clientRevision)
                            .setAcceptedSpecRevision(10L)
                            .setAppliedSpecRevision(9L)
                            .setPublishedGeneration(8L)
                            .build())
                    .build());
        }

        private static ProfileRef spacesProfile() {
            return ProfileRef.newBuilder()
                    .setProfileId(ProfileMetadata.SPACES_PROFILE_ID)
                    .setSchemaVersion(ProfileMetadata.SPACES_SCHEMA_VERSION)
                    .setDescriptorDigest(ProfileMetadata.SPACES_DESCRIPTOR_DIGEST)
                    .build();
        }
    }
}
