#![no_main]
//! Fuzz OCB2 packet decryption with a fixed key and IVs: hostile packet bytes
//! must never panic and must fail closed (return None) unless authentic.
use libfuzzer_sys::fuzz_target;
use mumble_server_runtime_crypto::CryptState;

fuzz_target!(|data: &[u8]| {
    let key = [0u8; 16];
    let iv = [0u8; 16];
    let mut state = CryptState::new(&key, &iv, &iv);
    let _ = state.decrypt(data);
});
