package be.theking90000.mumble.controller;

import be.theking90000.mumble.controller.internal.core.v1.ProfileCommand;
import be.theking90000.mumble.controller.internal.core.v1.ProfileEvent;
import be.theking90000.mumble.controller.internal.core.v1.ProfilePayload;
import be.theking90000.mumble.controller.internal.spaces.v1.Command;
import be.theking90000.mumble.controller.internal.spaces.v1.DesiredState;
import be.theking90000.mumble.controller.internal.spaces.v1.Event;
import com.google.protobuf.ByteString;
import com.google.protobuf.InvalidProtocolBufferException;
import com.google.protobuf.MessageLite;
import java.util.Set;

/** Typed Spaces payload codec layered over the generic Core envelopes. */
final class SpacesWire {
    private SpacesWire() {
    }

    static ProfilePayload participantSpec(ParticipantSpec spec) {
        return payload(be.theking90000.mumble.controller.internal.spaces.v1.ParticipantSpec
                .newBuilder()
                .setSpaceKey(spec.spaceKey().value())
                .setDisplayName(spec.displayName())
                .setServerMute(spec.serverMute())
                .setServerDeaf(spec.serverDeaf())
                .build());
    }

    static ProfilePayload desiredState(Set<SpaceKey> observedSpaces) {
        DesiredState.Builder state = DesiredState.newBuilder();
        for (SpaceKey spaceKey : observedSpaces) {
            state.addObservedSpaceKeys(spaceKey.value());
        }
        return payload(state.build());
    }

    static ProfileCommand command(ByteString sessionToken, Command command) {
        return ProfileCommand.newBuilder()
                .setSessionToken(sessionToken)
                .setPayload(payload(command))
                .build();
    }

    static DesiredState desiredState(ProfilePayload payload) {
        return parse(payload.getProtobuf(), DesiredState.parser());
    }

    static be.theking90000.mumble.controller.internal.spaces.v1.ParticipantSpec participantSpec(
            ProfilePayload payload) {
        return parse(
                payload.getProtobuf(),
                be.theking90000.mumble.controller.internal.spaces.v1.ParticipantSpec.parser());
    }

    static Command decodedCommand(ProfileCommand command) {
        return parse(command.getPayload().getProtobuf(), Command.parser());
    }

    static ProfileEvent profileEvent(Event event) {
        return ProfileEvent.newBuilder().setPayload(payload(event)).build();
    }

    static Event event(ProfileEvent event) {
        if (!event.hasPayload()) {
            throw new ControllerException("Spaces event payload is missing");
        }
        return parse(event.getPayload().getProtobuf(), Event.parser());
    }

    static be.theking90000.mumble.controller.internal.spaces.v1.ParticipantStatus status(
            be.theking90000.mumble.controller.internal.core.v1.ParticipantStatus status) {
        if (!status.hasProfileStatus()) {
            throw new ControllerException("Spaces participant status payload is missing");
        }
        return parse(
                status.getProfileStatus().getProtobuf(),
                be.theking90000.mumble.controller.internal.spaces.v1.ParticipantStatus.parser());
    }

    private static ProfilePayload payload(MessageLite message) {
        return ProfilePayload.newBuilder().setProtobuf(message.toByteString()).build();
    }

    private static <T> T parse(ByteString bytes, com.google.protobuf.Parser<T> parser) {
        try {
            return parser.parseFrom(bytes);
        } catch (InvalidProtocolBufferException failure) {
            throw new ControllerException("invalid typed Spaces payload", failure);
        }
    }
}
