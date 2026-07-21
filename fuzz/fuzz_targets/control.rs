#![no_main]
//! Fuzz control-message decoding: the first two bytes pick a message type code,
//! the rest is the payload. Every type (and unknown codes) must decode or fail
//! cleanly, never panic.
use libfuzzer_sys::fuzz_target;
use voxloom_protocol::decode_control;

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let message_type = u16::from_be_bytes([data[0], data[1]]);
    let _ = decode_control(message_type, &data[2..]);
});
