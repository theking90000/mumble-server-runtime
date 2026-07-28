#![no_main]
//! Fuzz UDP envelope decoding: arbitrary (already-decrypted) packet bytes must
//! decode or fail closed, never panic.
use libfuzzer_sys::fuzz_target;
use mumble_server_runtime_protocol::decode_udp;

fuzz_target!(|data: &[u8]| {
    let _ = decode_udp(data);
});
