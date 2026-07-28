//! Deterministic no-panic smoke over the decoders, runnable on stable in the
//! normal `cargo test` CI. It is a cheap proxy for the cargo-fuzz targets under
//! `fuzz/` (which need nightly): every decoder must return, never panic, on
//! arbitrary bytes (fail closed, L4). A crash here fails the test.

/// xorshift64 — a tiny deterministic PRNG, so failures reproduce exactly.
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
fn decoders_never_panic_on_arbitrary_bytes() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..100_000 {
        let data = rng.bytes(80);

        // Framing: drain until incomplete or error; must always terminate.
        let mut buffer = &data[..];
        while let Ok(Some(frame)) = mumble_server_runtime_protocol::parse_frame(buffer) {
            let consumed = frame.total_len();
            if consumed == 0 || consumed > buffer.len() {
                break;
            }
            buffer = &buffer[consumed..];
        }

        // Control-message decode: first two bytes select the type code.
        if data.len() >= 2 {
            let message_type = u16::from_be_bytes([data[0], data[1]]);
            let _ = mumble_server_runtime_protocol::decode_control(message_type, &data[2..]);
        }

        // UDP envelope decode.
        let _ = mumble_server_runtime_protocol::decode_udp(&data);
    }
}
