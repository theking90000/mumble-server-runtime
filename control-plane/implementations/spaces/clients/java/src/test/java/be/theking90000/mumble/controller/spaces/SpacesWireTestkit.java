package be.theking90000.mumble.controller.spaces;

import be.theking90000.mumble.controller.core.ControllerException;
import be.theking90000.mumble.controller.internal.core.v1.ProfileCommand;
import be.theking90000.mumble.controller.internal.core.v1.ProfileEvent;
import be.theking90000.mumble.controller.internal.core.v1.ProfilePayload;
import be.theking90000.mumble.controller.internal.spaces.v1.Command;
import be.theking90000.mumble.controller.internal.spaces.v1.DesiredState;
import be.theking90000.mumble.controller.internal.spaces.v1.Event;
import com.google.protobuf.ByteString;
import com.google.protobuf.InvalidProtocolBufferException;
import com.google.protobuf.MessageLite;

final class SpacesWireTestkit {
    private SpacesWireTestkit() {
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

    private static ProfilePayload payload(MessageLite message) {
        return ProfilePayload.newBuilder().setProtobuf(message.toByteString()).build();
    }

    private static <T> T parse(ByteString bytes, com.google.protobuf.Parser<T> parser) {
        try {
            return parser.parseFrom(bytes);
        } catch (InvalidProtocolBufferException failure) {
            throw new ControllerException("invalid typed Spaces test payload", failure);
        }
    }
}
