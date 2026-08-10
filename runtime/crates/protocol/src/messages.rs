//! Protobuf message types for the Mumble protocol, generated at build time from
//! the vendored `.proto` (see `build.rs`); never hand-transcribed (R1).
//!
//! REF: references/vendored/Mumble.proto (package MumbleProto) — TCP control messages.
//! REF: references/vendored/MumbleUDP.proto (package MumbleUDP) — UDP audio/ping envelope.
//!
//! Note the `UDPTunnel` message defined in Mumble.proto is "Not used" on the wire:
//! a TCP frame of type `UdpTunnel` carries raw audio bytes, not this message. That
//! special case lives in the UDP envelope handling, not here.

/// TCP control-channel messages (proto2, `package MumbleProto`).
pub mod tcp {
    include!(concat!(env!("OUT_DIR"), "/mumble_proto.rs"));
}

/// UDP voice-plane messages (proto3, `package MumbleUDP`).
pub mod udp {
    include!(concat!(env!("OUT_DIR"), "/mumble_udp.rs"));
}

#[cfg(test)]
mod tests {
    // See framing.rs for why the test module allows expect_used under -D warnings.
    #![allow(clippy::expect_used)]

    use super::*;
    use prost::Message;

    #[test]
    fn tcp_version_roundtrips() {
        let original = tcp::Version {
            version_v1: Some(0x0001_0500),
            version_v2: Some(0x0000_0001_0005_0000),
            release: Some("mumble-server-runtime-test".to_string()),
            os: Some("linux".to_string()),
            os_version: Some("6.1".to_string()),
        };
        let bytes = original.encode_to_vec();
        let decoded = tcp::Version::decode(&bytes[..]).expect("decode Version");
        assert_eq!(decoded, original);
    }

    #[test]
    fn tcp_empty_message_roundtrips() {
        // An all-unset proto2 message must survive an encode/decode cycle.
        let original = tcp::Version::default();
        let bytes = original.encode_to_vec();
        let decoded = tcp::Version::decode(&bytes[..]).expect("decode empty Version");
        assert_eq!(decoded, original);
        assert!(bytes.is_empty());
    }

    #[test]
    fn udp_audio_roundtrips_with_oneof_header() {
        let original = udp::Audio {
            header: Some(udp::audio::Header::Target(31)),
            sender_session: 42,
            frame_number: 7,
            opus_data: vec![1, 2, 3, 4],
            positional_data: vec![1.0, 2.0, 3.0],
            volume_adjustment: 0.5,
            is_terminator: true,
        };
        let bytes = original.encode_to_vec();
        let decoded = udp::Audio::decode(&bytes[..]).expect("decode Audio");
        assert_eq!(decoded, original);
    }

    #[test]
    fn udp_audio_context_variant_roundtrips() {
        // The other arm of the oneof must be preserved too.
        let original = udp::Audio {
            header: Some(udp::audio::Header::Context(2)),
            sender_session: 1,
            frame_number: 0,
            opus_data: vec![],
            positional_data: vec![],
            volume_adjustment: 0.0,
            is_terminator: false,
        };
        let bytes = original.encode_to_vec();
        let decoded = udp::Audio::decode(&bytes[..]).expect("decode Audio");
        assert_eq!(decoded, original);
        assert_eq!(decoded.header, Some(udp::audio::Header::Context(2)));
    }

    #[test]
    fn udp_ping_roundtrips() {
        let original = udp::Ping {
            timestamp: 123_456_789,
            request_extended_information: true,
            server_version_v2: 0x0000_0001_0005_0000,
            user_count: 3,
            max_user_count: 100,
            max_bandwidth_per_user: 128_000,
        };
        let bytes = original.encode_to_vec();
        let decoded = udp::Ping::decode(&bytes[..]).expect("decode Ping");
        assert_eq!(decoded, original);
    }
}
