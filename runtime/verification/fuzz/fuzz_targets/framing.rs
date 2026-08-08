#![no_main]
//! Fuzz the TCP framer: arbitrary bytes must never panic, and draining frames
//! must always terminate.
use libfuzzer_sys::fuzz_target;
use mumble_server_runtime_protocol::parse_frame;

fuzz_target!(|data: &[u8]| {
    let mut buffer = data;
    while let Ok(Some(frame)) = parse_frame(buffer) {
        let consumed = frame.total_len();
        // total_len is header + payload, always >= 6 and <= buffer.len() for a
        // parsed frame; guard anyway so the loop cannot spin or slice OOB.
        if consumed == 0 || consumed > buffer.len() {
            break;
        }
        buffer = &buffer[consumed..];
    }
});
