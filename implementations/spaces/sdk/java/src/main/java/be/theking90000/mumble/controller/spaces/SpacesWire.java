package be.theking90000.mumble.controller.spaces;

import be.theking90000.mumble.controller.core.ControllerException;
import be.theking90000.mumble.controller.core.CoreParticipantStatus;
import be.theking90000.mumble.controller.core.ProfilePayload;

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

    static ProfilePayload command(Command command) {
        return payload(command);
    }

    static Event event(ProfilePayload event) {
        return parse(com.google.protobuf.ByteString.copyFrom(event.protobuf()), Event.parser());
    }

    static be.theking90000.mumble.controller.internal.spaces.v1.ParticipantStatus status(
            CoreParticipantStatus status) {
        return parse(
                ByteString.copyFrom(status.profileStatus().protobuf()),
                be.theking90000.mumble.controller.internal.spaces.v1.ParticipantStatus.parser());
    }

    private static ProfilePayload payload(MessageLite message) {
        return ProfilePayload.of(message.toByteArray());
    }

    private static <T> T parse(ByteString bytes, com.google.protobuf.Parser<T> parser) {
        try {
            return parser.parseFrom(bytes);
        } catch (InvalidProtocolBufferException failure) {
            throw new ControllerException("invalid typed Spaces payload", failure);
        }
    }
}
