#![no_main]
//! Fuzz the control-message encode/decode round trip. Any bytes that decode must,
//! once re-encoded, decode again and re-encode to the *same bytes*. This is the
//! stability the MITM proxy depends on: what it forwards (`encode(decode(x))`) is
//! a fixed point, so a peer re-decoding it sees a stable message.
//!
//! Byte stability, not message equality, is asserted on purpose: float fields
//! (e.g. `Ping.tcp_ping_var`) can hold NaN, and `NaN != NaN` would make message
//! equality spuriously fail even though encode/decode are proper inverses.
use libfuzzer_sys::fuzz_target;
use mumble_server_runtime_protocol::{decode_control, encode_control};

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let message_type = u16::from_be_bytes([data[0], data[1]]);
    if let Ok(message) = decode_control(message_type, &data[2..]) {
        let first = encode_control(&message);
        let redecoded =
            decode_control(first.0, &first.1).expect("re-decode of our own encoding");
        let second = encode_control(&redecoded);
        assert_eq!(first, second);
    }
});
