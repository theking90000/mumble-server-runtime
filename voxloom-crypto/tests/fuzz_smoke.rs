//! Deterministic no-panic smoke over OCB2 packet decryption, runnable on stable
//! in the normal `cargo test` CI. A cheap proxy for the nightly cargo-fuzz
//! `ocb2_decrypt` target: hostile packet bytes must never panic and must fail
//! closed (return None) unless authentic. A crash here fails the test.

use voxloom_crypto::CryptState;

/// xorshift64 — deterministic PRNG for reproducible inputs.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn bytes(&mut self, max_len: usize) -> Vec<u8> {
        let len = (self.next_u64() as usize) % (max_len + 1);
        (0..len).map(|_| self.next_u64() as u8).collect()
    }
}

#[test]
fn decrypt_never_panics_on_arbitrary_packets() {
    let key = [0x11u8; 16];
    let encrypt_iv = [0x22u8; 16];
    let decrypt_iv = [0x33u8; 16];
    let mut rng = Rng(0x0DDB_1A5E_5BAD_F00D);

    let mut state = CryptState::new(&key, &encrypt_iv, &decrypt_iv);
    for _ in 0..100_000 {
        let packet = rng.bytes(80);
        // Random bytes are overwhelmingly not authentic; the point is no panic.
        let _ = state.decrypt(&packet);
    }
}
