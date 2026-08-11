package be.theking90000.mumble.controller;

import be.theking90000.mumble.controller.internal.core.v1.ProfilePayload;
import be.theking90000.mumble.controller.internal.spaces.v1.Command;
import be.theking90000.mumble.controller.internal.spaces.v1.DesiredState;
import be.theking90000.mumble.controller.internal.spaces.v1.Event;
import com.google.protobuf.ByteString;
import com.google.protobuf.InvalidProtocolBufferException;

/** Bridges the current Spaces facade to the language-independent Core envelopes. */
final class WireAdapter {
    private WireAdapter() {
    }

    static be.theking90000.mumble.controller.internal.core.v1.ClientFrame toCore(
            be.theking90000.mumble.controller.internal.protocol.v1.ClientFrame frame) {
        be.theking90000.mumble.controller.internal.core.v1.ClientFrame.Builder result =
                be.theking90000.mumble.controller.internal.core.v1.ClientFrame.newBuilder()
                        .setRequestId(frame.getRequestId());
        switch (frame.getPayloadCase()) {
            case OPEN_SESSION:
                result.setOpenSession(toCore(frame.getOpenSession()));
                break;
            case RENEW_LEASE:
                result.setRenewLease(
                        be.theking90000.mumble.controller.internal.core.v1.RenewLease.newBuilder()
                                .setSessionToken(frame.getRenewLease().getSessionToken())
                                .setDesiredStateRevision(
                                        frame.getRenewLease().getDesiredStateRevision()));
                break;
            case SYNC_DESIRED_STATE:
                result.setSyncDesiredState(
                        be.theking90000.mumble.controller.internal.core.v1.SyncDesiredState
                                .newBuilder()
                                .setSessionToken(frame.getSyncDesiredState().getSessionToken())
                                .setDesiredState(toCore(frame.getSyncDesiredState().getDesiredState())));
                break;
            case REGISTER_PARTICIPANT:
                result.setRegisterParticipant(
                        be.theking90000.mumble.controller.internal.core.v1.RegisterParticipant
                                .newBuilder()
                                .setSessionToken(frame.getRegisterParticipant().getSessionToken())
                                .setParticipant(toCore(
                                        frame.getRegisterParticipant().getParticipant())));
                break;
            case SET_PARTICIPANT_SPEC:
                result.setSetParticipantSpec(
                        be.theking90000.mumble.controller.internal.core.v1.SetParticipantSpec
                                .newBuilder()
                                .setSessionToken(frame.getSetParticipantSpec().getSessionToken())
                                .setParticipantId(frame.getSetParticipantSpec().getParticipantId())
                                .setOwnershipToken(frame.getSetParticipantSpec().getOwnershipToken())
                                .setClientSpecRevision(
                                        frame.getSetParticipantSpec().getClientSpecRevision())
                                .setProfileSpec(payload(
                                        frame.getSetParticipantSpec().getSpec().toByteString())));
                break;
            case RELEASE_PARTICIPANT:
                result.setReleaseParticipant(
                        be.theking90000.mumble.controller.internal.core.v1.ReleaseParticipant
                                .newBuilder()
                                .setSessionToken(frame.getReleaseParticipant().getSessionToken())
                                .setParticipantId(frame.getReleaseParticipant().getParticipantId())
                                .setRegistrationId(
                                        frame.getReleaseParticipant().getRegistrationId())
                                .setOwnershipToken(
                                        frame.getReleaseParticipant().getOwnershipToken()));
                break;
            case REPLACE_OBSERVED_SPACES:
                result.setProfileCommand(profileCommand(
                        frame.getReplaceObservedSpaces().getSessionToken(),
                        Command.newBuilder()
                                .setReplaceObservedSpaces(
                                        be.theking90000.mumble.controller.internal.spaces.v1
                                                .ReplaceObservedSpaces.newBuilder()
                                                .setObservedSpacesRevision(frame
                                                        .getReplaceObservedSpaces()
                                                        .getObservedSpacesRevision())
                                                .addAllSpaceKeys(frame
                                                        .getReplaceObservedSpaces()
                                                        .getSpaceKeysList()))
                                .build()));
                break;
            case FETCH_SPACE:
                result.setProfileCommand(profileCommand(
                        frame.getFetchSpace().getSessionToken(),
                        Command.newBuilder()
                                .setFetchSpace(
                                        be.theking90000.mumble.controller.internal.spaces.v1
                                                .FetchSpace.newBuilder()
                                                .setSpaceKey(frame.getFetchSpace().getSpaceKey()))
                                .build()));
                break;
            case CLOSE_SESSION:
                result.setCloseSession(
                        be.theking90000.mumble.controller.internal.core.v1.CloseSession.newBuilder()
                                .setSessionToken(frame.getCloseSession().getSessionToken()));
                break;
            case PAYLOAD_NOT_SET:
            default:
                throw new ControllerException("ClientFrame has no supported payload");
        }
        return result.build();
    }

    static be.theking90000.mumble.controller.internal.protocol.v1.ServerFrame fromCore(
            be.theking90000.mumble.controller.internal.core.v1.ServerFrame frame) {
        be.theking90000.mumble.controller.internal.protocol.v1.ServerFrame.Builder result =
                be.theking90000.mumble.controller.internal.protocol.v1.ServerFrame.newBuilder()
                        .setRequestId(frame.getRequestId());
        switch (frame.getPayloadCase()) {
            case SESSION_READY:
                result.setSessionReady(fromCore(frame.getSessionReady()));
                break;
            case DESIRED_STATE_RECONCILED:
                result.setDesiredStateReconciled(parse(
                        frame.getDesiredStateReconciled().toByteString(),
                        be.theking90000.mumble.controller.internal.protocol.v1
                                .DesiredStateReconciled.parser()));
                break;
            case PARTICIPANT_OWNERSHIP_GRANTED:
                result.setParticipantOwnershipGranted(parse(
                        frame.getParticipantOwnershipGranted().toByteString(),
                        be.theking90000.mumble.controller.internal.protocol.v1
                                .ParticipantOwnershipGranted.parser()));
                break;
            case PARTICIPANT_OWNERSHIP_REVOKED:
                result.setParticipantOwnershipRevoked(parse(
                        frame.getParticipantOwnershipRevoked().toByteString(),
                        be.theking90000.mumble.controller.internal.protocol.v1
                                .ParticipantOwnershipRevoked.parser()));
                break;
            case PARTICIPANT_SPEC_ACCEPTED:
                result.setParticipantSpecAccepted(parse(
                        frame.getParticipantSpecAccepted().toByteString(),
                        be.theking90000.mumble.controller.internal.protocol.v1
                                .ParticipantSpecAccepted.parser()));
                break;
            case PARTICIPANT_STATUS_CHANGED:
                result.setParticipantStatusChanged(fromCore(frame.getParticipantStatusChanged()));
                break;
            case PROFILE_EVENT:
                applyProfileEvent(result, decodeEvent(frame.getProfileEvent().getPayload()));
                break;
            case RESYNC_REQUIRED:
                result.setResyncRequired(parse(
                        frame.getResyncRequired().toByteString(),
                        be.theking90000.mumble.controller.internal.protocol.v1.ResyncRequired
                                .parser()));
                break;
            case COMMAND_REJECTED:
                result.setCommandRejected(
                        be.theking90000.mumble.controller.internal.protocol.v1.CommandRejected
                                .newBuilder()
                                .setCodeValue(frame.getCommandRejected().getCodeValue())
                                .setMessage(frame.getCommandRejected().getMessage()));
                break;
            case SESSION_CLOSING:
                result.setSessionClosing(parse(
                        frame.getSessionClosing().toByteString(),
                        be.theking90000.mumble.controller.internal.protocol.v1.SessionClosing
                                .parser()));
                break;
            case PAYLOAD_NOT_SET:
            default:
                throw new ControllerException("ServerFrame has no supported payload");
        }
        return result.build();
    }

    private static be.theking90000.mumble.controller.internal.core.v1.OpenSession toCore(
            be.theking90000.mumble.controller.internal.protocol.v1.OpenSession message) {
        be.theking90000.mumble.controller.internal.core.v1.OpenSession.Builder result =
                be.theking90000.mumble.controller.internal.core.v1.OpenSession.newBuilder()
                        .setControllerId(message.getControllerId())
                        .setControllerInstanceId(message.getControllerInstanceId())
                        .setResumeToken(message.getResumeToken())
                        .setProfile(be.theking90000.mumble.controller.internal.core.v1.ProfileRef
                                .newBuilder()
                                .setProfileId(message.getProfile().getProfileId())
                                .setSchemaVersion(message.getProfile().getSchemaVersion())
                                .setDescriptorDigest(message.getProfile().getDescriptorDigest()));
        if (message.hasDesiredState()) {
            result.setDesiredState(toCore(message.getDesiredState()));
        }
        return result.build();
    }

    private static be.theking90000.mumble.controller.internal.core.v1.DesiredStateSnapshot toCore(
            be.theking90000.mumble.controller.internal.protocol.v1.DesiredStateSnapshot snapshot) {
        be.theking90000.mumble.controller.internal.core.v1.DesiredStateSnapshot.Builder result =
                be.theking90000.mumble.controller.internal.core.v1.DesiredStateSnapshot.newBuilder()
                        .setDesiredStateRevision(snapshot.getDesiredStateRevision())
                        .setProfileState(payload(DesiredState.newBuilder()
                                .addAllObservedSpaceKeys(snapshot.getObservedSpaceKeysList())
                                .build()
                                .toByteString()));
        for (be.theking90000.mumble.controller.internal.protocol.v1.ParticipantRegistration
                registration : snapshot.getParticipantsList()) {
            result.addParticipants(toCore(registration));
        }
        return result.build();
    }

    private static be.theking90000.mumble.controller.internal.core.v1.ParticipantRegistration
            toCore(
                    be.theking90000.mumble.controller.internal.protocol.v1.ParticipantRegistration
                            registration) {
        return be.theking90000.mumble.controller.internal.core.v1.ParticipantRegistration
                .newBuilder()
                .setParticipantId(registration.getParticipantId())
                .setRegistrationId(registration.getRegistrationId())
                .setOwnershipToken(registration.getOwnershipToken())
                .setClientSpecRevision(registration.getClientSpecRevision())
                .setProfileSpec(payload(registration.getSpec().toByteString()))
                .build();
    }

    private static be.theking90000.mumble.controller.internal.core.v1.ProfileCommand profileCommand(
            ByteString sessionToken, Command command) {
        return be.theking90000.mumble.controller.internal.core.v1.ProfileCommand.newBuilder()
                .setSessionToken(sessionToken)
                .setPayload(payload(command.toByteString()))
                .build();
    }

    private static ProfilePayload payload(ByteString bytes) {
        return ProfilePayload.newBuilder().setProtobuf(bytes).build();
    }

    private static be.theking90000.mumble.controller.internal.protocol.v1.SessionReady fromCore(
            be.theking90000.mumble.controller.internal.core.v1.SessionReady message) {
        be.theking90000.mumble.controller.internal.protocol.v1.SessionReady.Builder result =
                be.theking90000.mumble.controller.internal.protocol.v1.SessionReady.newBuilder()
                        .setSessionToken(message.getSessionToken())
                        .setResumeToken(message.getResumeToken())
                        .setControlEpoch(message.getControlEpoch())
                        .setProfile(be.theking90000.mumble.controller.internal.protocol.v1.ProfileRef
                                .newBuilder()
                                .setProfileId(message.getProfile().getProfileId())
                                .setSchemaVersion(message.getProfile().getSchemaVersion())
                                .setDescriptorDigest(message.getProfile().getDescriptorDigest()));
        if (message.hasLeaseDuration()) {
            result.setLeaseDuration(message.getLeaseDuration());
        }
        return result.build();
    }

    private static be.theking90000.mumble.controller.internal.protocol.v1
            .ParticipantStatusChanged fromCore(
                    be.theking90000.mumble.controller.internal.core.v1
                            .ParticipantStatusChanged message) {
        be.theking90000.mumble.controller.internal.core.v1.ParticipantStatus status =
                message.getStatus();
        be.theking90000.mumble.controller.internal.spaces.v1.ParticipantStatus spacesStatus =
                parse(status.getProfileStatus().getProtobuf(),
                        be.theking90000.mumble.controller.internal.spaces.v1.ParticipantStatus
                                .parser());
        return be.theking90000.mumble.controller.internal.protocol.v1.ParticipantStatusChanged
                .newBuilder()
                .setParticipantId(message.getParticipantId())
                .setStatus(be.theking90000.mumble.controller.internal.protocol.v1.ParticipantStatus
                        .newBuilder()
                        .setMumbleConnected(status.getMumbleConnected())
                        .setAppliedSpaceKey(spacesStatus.getAppliedSpaceKey())
                        .setSelfMute(spacesStatus.getSelfMute())
                        .setSelfDeaf(spacesStatus.getSelfDeaf())
                        .setAcceptedSpecRevision(status.getAcceptedSpecRevision())
                        .setAppliedSpecRevision(status.getAppliedSpecRevision())
                        .setPublishedGeneration(status.getPublishedGeneration())
                        .setApplicationError(status.getApplicationError()))
                .build();
    }

    private static Event decodeEvent(ProfilePayload payload) {
        return parse(payload.getProtobuf(), Event.parser());
    }

    private static void applyProfileEvent(
            be.theking90000.mumble.controller.internal.protocol.v1.ServerFrame.Builder result,
            Event event) {
        switch (event.getEventCase()) {
            case OBSERVED_SPACES_ACCEPTED:
                result.setObservedSpacesAccepted(parse(
                        event.getObservedSpacesAccepted().toByteString(),
                        be.theking90000.mumble.controller.internal.protocol.v1
                                .ObservedSpacesAccepted.parser()));
                break;
            case SPACE_SNAPSHOT:
                result.setSpaceSnapshot(parse(
                        event.getSpaceSnapshot().toByteString(),
                        be.theking90000.mumble.controller.internal.protocol.v1.SpaceSnapshot
                                .parser()));
                break;
            case SPACE_CLOSED:
                result.setSpaceClosed(parse(
                        event.getSpaceClosed().toByteString(),
                        be.theking90000.mumble.controller.internal.protocol.v1.SpaceClosed
                                .parser()));
                break;
            case FETCH_SPACE_RESULT:
                result.setFetchSpaceResult(parse(
                        event.getFetchSpaceResult().toByteString(),
                        be.theking90000.mumble.controller.internal.protocol.v1.FetchSpaceResult
                                .parser()));
                break;
            case EVENT_NOT_SET:
            default:
                throw new ControllerException("Spaces Event has no supported event");
        }
    }

    private static <T> T parse(ByteString bytes, com.google.protobuf.Parser<T> parser) {
        try {
            return parser.parseFrom(bytes);
        } catch (InvalidProtocolBufferException failure) {
            throw new ControllerException("invalid typed Spaces payload", failure);
        }
    }
}
