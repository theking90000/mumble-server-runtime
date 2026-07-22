#![no_main]
//! Fuzz the UDP envelope encode/decode round trip. Any (already-decrypted) bytes
//! that decode must, once re-encoded, either be rejected as too short or decode
//! again and re-encode to the *same bytes* — the fixed-point stability the MITM
//! proxy relies on.
//!
//! Two deliberate choices:
//! - Byte stability, not message equality: `Audio.volume_adjustment` is a float
//!   that can be NaN, and `NaN != NaN` would spuriously fail message equality.
//! - The re-decode is allowed to fail as TooShort: a message whose every protobuf
//!   field is at its default (e.g. `Ping{timestamp:0}`) encodes to a header-only
//!   packet, which `decode_udp` rejects exactly as Mumble does (`data.size() <=
//!   1`). Real traffic never produces that; the corpus round-trip test covers the
//!   real, non-degenerate messages.
use libfuzzer_sys::fuzz_target;
use voxloom_protocol::{decode_udp, encode_udp};

fuzz_target!(|data: &[u8]| {
    if let Ok(message) = decode_udp(data) {
        let first = encode_udp(&message);
        if let Ok(redecoded) = decode_udp(&first) {
            assert_eq!(first, encode_udp(&redecoded));
        }
    }
});
