//! OCB2-AES128 as implemented by Mumble, including its mitigations against the
//! attack in <https://eprint.iacr.org/2019/311> (section 9 / XEX* forgery).
//!
//! Ported from the vendored reference (R1); every routine mirrors its C++ source:
//!
//! REF: references/vendored/ocb2-vectors/CryptStateOCB2.cpp : `ocb_encrypt`,
//!      `ocb_decrypt`, `encrypt`, `decrypt`, `S2`, `S3`, `XOR`.
//! REF: references/vendored/ocb2-vectors/TestCrypt.cpp : the golden vectors and
//!      mitigation tests reproduced in this module's `tests`.
//!
//! The block "doubling" (`S2`) and "tripling" (`S3`) treat the 16-byte block as a
//! big-endian 128-bit element of GF(2^128) with reduction polynomial 0x87; the
//! reference's endianness dance (`SWAPPED`) is just that operation, written here
//! portably in terms of bytes.

use aes::Aes128;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};

/// AES-128 block size in bytes.
pub const BLOCK_SIZE: usize = 16;
/// AES-128 key size in bytes.
pub const KEY_SIZE: usize = 16;

/// A single 16-byte block.
type Block = [u8; BLOCK_SIZE];

/// XOR two blocks.
/// REF: CryptStateOCB2.cpp : `XOR`.
fn xor(a: &Block, b: &Block) -> Block {
    let mut out = [0u8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        out[i] = a[i] ^ b[i];
    }
    out
}

/// Double a block in GF(2^128): left-shift the big-endian 128-bit value by one
/// bit, reducing with 0x87 when the top bit was set.
/// REF: CryptStateOCB2.cpp : `S2`.
fn times2(block: &mut Block) {
    let carry = block[0] >> 7;
    for i in 0..BLOCK_SIZE - 1 {
        block[i] = (block[i] << 1) | (block[i + 1] >> 7);
    }
    block[BLOCK_SIZE - 1] = (block[BLOCK_SIZE - 1] << 1) ^ (carry * 0x87);
}

/// Triple a block in GF(2^128): `3*x == x ^ 2*x`.
/// REF: CryptStateOCB2.cpp : `S3` (`block ^= <double>`).
fn times3(block: &mut Block) {
    let mut doubled = *block;
    times2(&mut doubled);
    for i in 0..BLOCK_SIZE {
        block[i] ^= doubled[i];
    }
}

/// AES-128 encrypt one block.
fn aes_encrypt(cipher: &Aes128, input: &Block) -> Block {
    let mut buffer = GenericArray::clone_from_slice(input);
    cipher.encrypt_block(&mut buffer);
    let mut out = [0u8; BLOCK_SIZE];
    out.copy_from_slice(buffer.as_slice());
    out
}

/// AES-128 decrypt one block.
fn aes_decrypt(cipher: &Aes128, input: &Block) -> Block {
    let mut buffer = GenericArray::clone_from_slice(input);
    cipher.decrypt_block(&mut buffer);
    let mut out = [0u8; BLOCK_SIZE];
    out.copy_from_slice(buffer.as_slice());
    out
}

/// The 16-byte block encoding the final-block bit length (`len * 8`), big-endian.
/// REF: CryptStateOCB2.cpp : `tmp[BLOCKSIZE - 1] = SWAPPED(len * 8)`.
fn length_block(len: usize) -> Block {
    // Widening usize -> u128 is lossless and `* 8` cannot overflow u128 for any
    // real length; the fallback is unreachable and kept only to avoid `as`.
    let bits = u128::try_from(len).unwrap_or(u128::MAX).wrapping_mul(8);
    bits.to_be_bytes()
}

/// OCB2 encryption of `plain` under `cipher` with the given `nonce`.
///
/// Returns `(authentic, ciphertext, tag)`. `authentic` is false only when a
/// XEX*-critical block is detected and `modify_on_xexstar_attack` is false — the
/// path used only to exercise [`ocb_decrypt`]'s detection. With the flag true
/// (the production setting), a critical block has a bit flipped so encryption
/// stays authentic while defeating the attack.
///
/// REF: CryptStateOCB2.cpp : `ocb_encrypt`.
pub fn ocb_encrypt(
    cipher: &Aes128,
    plain: &[u8],
    nonce: &Block,
    modify_on_xexstar_attack: bool,
) -> (bool, Vec<u8>, Block) {
    let mut authentic = true;
    let mut delta = aes_encrypt(cipher, nonce);
    let mut checksum = [0u8; BLOCK_SIZE];
    let mut encrypted = Vec::with_capacity(plain.len());

    let mut offset = 0;
    let mut remaining = plain.len();
    while remaining > BLOCK_SIZE {
        let block = read_block(plain, offset);

        // Counter-cryptanalysis (section 9 of eprint 2019/311): the second-to-last
        // block being all zero except possibly its last byte is XEX*-critical.
        let mut flip_a_bit = false;
        if remaining - BLOCK_SIZE <= BLOCK_SIZE {
            let mut sum = 0u8;
            for &byte in &block[..BLOCK_SIZE - 1] {
                sum |= byte;
            }
            if sum == 0 {
                if modify_on_xexstar_attack {
                    flip_a_bit = true;
                } else {
                    authentic = false;
                }
            }
        }

        times2(&mut delta);
        let mut tmp = xor(&delta, &block);
        if flip_a_bit {
            tmp[0] ^= 1;
        }
        tmp = aes_encrypt(cipher, &tmp);
        encrypted.extend_from_slice(&xor(&delta, &tmp));

        checksum = xor(&checksum, &block);
        if flip_a_bit {
            checksum[0] ^= 1;
        }

        offset += BLOCK_SIZE;
        remaining -= BLOCK_SIZE;
    }

    // Final (possibly partial) block.
    times2(&mut delta);
    let pad = aes_encrypt(cipher, &xor(&length_block(remaining), &delta));

    let mut tail = pad;
    tail[..remaining].copy_from_slice(&plain[offset..offset + remaining]);
    checksum = xor(&checksum, &tail);
    let out_tail = xor(&pad, &tail);
    encrypted.extend_from_slice(&out_tail[..remaining]);

    times3(&mut delta);
    let tag = aes_encrypt(cipher, &xor(&delta, &checksum));

    (authentic, encrypted, tag)
}

/// OCB2 decryption of `encrypted` under `cipher` with the given `nonce`.
///
/// Returns `(authentic, plaintext, tag)`. The caller must still compare `tag`
/// against the transmitted tag; `authentic` additionally goes false when the
/// decrypted final block matches the XEX*-critical shape, which no honest packet
/// produces.
///
/// REF: CryptStateOCB2.cpp : `ocb_decrypt`.
pub fn ocb_decrypt(cipher: &Aes128, encrypted: &[u8], nonce: &Block) -> (bool, Vec<u8>, Block) {
    let mut authentic = true;
    let mut delta = aes_encrypt(cipher, nonce);
    let mut checksum = [0u8; BLOCK_SIZE];
    let mut plain = Vec::with_capacity(encrypted.len());

    let mut offset = 0;
    let mut remaining = encrypted.len();
    while remaining > BLOCK_SIZE {
        let block = read_block(encrypted, offset);
        times2(&mut delta);
        let tmp = aes_decrypt(cipher, &xor(&delta, &block));
        let decoded = xor(&delta, &tmp);
        plain.extend_from_slice(&decoded);
        checksum = xor(&checksum, &decoded);
        offset += BLOCK_SIZE;
        remaining -= BLOCK_SIZE;
    }

    // Final (possibly partial) block.
    times2(&mut delta);
    let pad = aes_encrypt(cipher, &xor(&length_block(remaining), &delta));

    let mut tail = [0u8; BLOCK_SIZE];
    tail[..remaining].copy_from_slice(&encrypted[offset..offset + remaining]);
    tail = xor(&tail, &pad);
    checksum = xor(&checksum, &tail);
    plain.extend_from_slice(&tail[..remaining]);

    // XEX* forgery detection: an attack needs the decrypted last block to equal
    // `delta` in every byte the length field does not touch (all but the last).
    if tail[..BLOCK_SIZE - 1] == delta[..BLOCK_SIZE - 1] {
        authentic = false;
    }

    times3(&mut delta);
    let tag = aes_encrypt(cipher, &xor(&delta, &checksum));

    (authentic, plain, tag)
}

/// Read the 16-byte block at `offset`. The callers only reach this with at least
/// a full block available, but it is written to never index out of bounds.
fn read_block(data: &[u8], offset: usize) -> Block {
    let mut block = [0u8; BLOCK_SIZE];
    if let Some(slice) = data.get(offset..offset + BLOCK_SIZE) {
        block.copy_from_slice(slice);
    }
    block
}

/// A full OCB2 crypt state for one direction pair: the AES key, both IVs, the
/// replay history and the good/late/lost counters. Mirrors Mumble's
/// `CryptStateOCB2` object (minus key generation, which belongs to session setup).
///
/// REF: CryptStateOCB2.cpp : `CryptStateOCB2`, `encrypt`, `decrypt`.
pub struct CryptState {
    cipher: Aes128,
    encrypt_iv: Block,
    decrypt_iv: Block,
    decrypt_history: [u8; 256],
    /// Count of successfully decrypted packets.
    pub good: u32,
    /// Running count of late (out-of-order but recovered) packets.
    pub late: u32,
    /// Running count of lost packets inferred from IV gaps.
    pub lost: u32,
}

impl CryptState {
    /// Build a state from a raw key and the two initial IVs.
    pub fn new(raw_key: &[u8; KEY_SIZE], encrypt_iv: &Block, decrypt_iv: &Block) -> Self {
        Self {
            cipher: Aes128::new(GenericArray::from_slice(raw_key)),
            encrypt_iv: *encrypt_iv,
            decrypt_iv: *decrypt_iv,
            decrypt_history: [0u8; 256],
            good: 0,
            late: 0,
            lost: 0,
        }
    }

    /// Current encrypt IV (mainly for tests that force/inspect IV state).
    pub fn encrypt_iv(&self) -> Block {
        self.encrypt_iv
    }

    /// Current decrypt IV.
    pub fn decrypt_iv(&self) -> Block {
        self.decrypt_iv
    }

    /// Overwrite the encrypt IV.
    pub fn set_encrypt_iv(&mut self, iv: &Block) {
        self.encrypt_iv = *iv;
    }

    /// Overwrite the decrypt IV.
    pub fn set_decrypt_iv(&mut self, iv: &Block) {
        self.decrypt_iv = *iv;
    }

    /// Encrypt a packet: advance the IV, OCB2-encrypt, and prepend the 4-byte
    /// header (IV low byte plus three tag bytes). Returns `None` only if
    /// encryption reports inauthenticity, which cannot happen here because the
    /// XEX* mitigation is enabled.
    ///
    /// REF: CryptStateOCB2.cpp : `CryptStateOCB2::encrypt`.
    pub fn encrypt(&mut self, plain: &[u8]) -> Option<Vec<u8>> {
        increment_iv(&mut self.encrypt_iv);
        let (authentic, ciphertext, tag) = ocb_encrypt(&self.cipher, plain, &self.encrypt_iv, true);
        if !authentic {
            return None;
        }
        let mut packet = Vec::with_capacity(4 + ciphertext.len());
        packet.push(self.encrypt_iv[0]);
        packet.extend_from_slice(&tag[..3]);
        packet.extend_from_slice(&ciphertext);
        Some(packet)
    }

    /// Decrypt a packet, performing IV recovery for out-of-order/lost packets and
    /// rejecting replays. Returns the plaintext, or `None` if the packet is a
    /// replay, fails IV recovery, or fails tag verification (fail closed, R6).
    ///
    /// REF: CryptStateOCB2.cpp : `CryptStateOCB2::decrypt`.
    pub fn decrypt(&mut self, source: &[u8]) -> Option<Vec<u8>> {
        if source.len() < 4 {
            return None;
        }
        let (header, ciphertext) = source.split_at(4);
        let ivbyte = header[0];
        let saveiv = self.decrypt_iv;
        let mut restore = false;
        let mut late: i32 = 0;
        let mut lost: i32 = 0;

        if self.decrypt_iv[0].wrapping_add(1) == ivbyte {
            // In order as expected.
            if ivbyte > self.decrypt_iv[0] {
                self.decrypt_iv[0] = ivbyte;
            } else if ivbyte < self.decrypt_iv[0] {
                self.decrypt_iv[0] = ivbyte;
                increment_iv_from(&mut self.decrypt_iv, 1);
            } else {
                return None;
            }
        } else {
            // Either out of order or a repeat.
            let mut diff = ivbyte as i32 - self.decrypt_iv[0] as i32;
            if diff > 128 {
                diff -= 256;
            } else if diff < -128 {
                diff += 256;
            }

            if ivbyte < self.decrypt_iv[0] && diff > -30 && diff < 0 {
                // Late packet, but no wraparound.
                late = 1;
                lost = -1;
                self.decrypt_iv[0] = ivbyte;
                restore = true;
            } else if ivbyte > self.decrypt_iv[0] && diff > -30 && diff < 0 {
                // Last was e.g. 0x02, here comes 0xff from the previous round.
                late = 1;
                lost = -1;
                self.decrypt_iv[0] = ivbyte;
                decrement_iv_from(&mut self.decrypt_iv, 1);
                restore = true;
            } else if ivbyte > self.decrypt_iv[0] && diff > 0 {
                // Lost a few packets, but beyond that we are good.
                lost = ivbyte as i32 - self.decrypt_iv[0] as i32 - 1;
                self.decrypt_iv[0] = ivbyte;
            } else if ivbyte < self.decrypt_iv[0] && diff > 0 {
                // Lost a few packets, and wrapped around.
                lost = 256 - self.decrypt_iv[0] as i32 + ivbyte as i32 - 1;
                self.decrypt_iv[0] = ivbyte;
                increment_iv_from(&mut self.decrypt_iv, 1);
            } else {
                return None;
            }

            if self.decrypt_history[self.decrypt_iv[0] as usize] == self.decrypt_iv[1] {
                self.decrypt_iv = saveiv;
                return None;
            }
        }

        let (authentic, plain, tag) = ocb_decrypt(&self.cipher, ciphertext, &self.decrypt_iv);
        if !authentic || tag[..3] != header[1..4] {
            self.decrypt_iv = saveiv;
            return None;
        }

        self.decrypt_history[self.decrypt_iv[0] as usize] = self.decrypt_iv[1];
        if restore {
            self.decrypt_iv = saveiv;
        }

        self.good = self.good.wrapping_add(1);
        apply_stat(&mut self.late, late);
        apply_stat(&mut self.lost, lost);
        Some(plain)
    }
}

/// Increment the IV as a little-endian 128-bit counter (carry from byte 0 up).
/// REF: CryptStateOCB2.cpp : `for (i) if (++encrypt_iv[i]) break;`.
fn increment_iv(iv: &mut Block) {
    increment_iv_from(iv, 0);
}

fn increment_iv_from(iv: &mut Block, start: usize) {
    for byte in iv.iter_mut().skip(start) {
        *byte = byte.wrapping_add(1);
        if *byte != 0 {
            break;
        }
    }
}

/// Borrow-decrement the IV from `start`, mirroring the reference post-decrement
/// (`if (decrypt_iv[i]--) break;` breaks when the pre-decrement value was nonzero).
fn decrement_iv_from(iv: &mut Block, start: usize) {
    for byte in iv.iter_mut().skip(start) {
        let was = *byte;
        *byte = byte.wrapping_sub(1);
        if was != 0 {
            break;
        }
    }
}

/// Apply a signed delta to an unsigned counter without wrapping below zero, as
/// the reference does for its late/lost statistics.
/// REF: CryptStateOCB2.cpp : the `uiLate`/`uiLost` update at the end of `decrypt`.
fn apply_stat(counter: &mut u32, delta: i32) {
    if delta > 0 {
        *counter = counter.wrapping_add(delta as u32);
    } else if (*counter as i32) > delta.abs() {
        *counter -= delta.unsigned_abs();
    }
}

#[cfg(test)]
mod tests {
    // Test asserts use expect() on Options; see mumble-server-runtime-protocol/framing.rs for
    // why this is allowed under -D warnings.
    #![allow(clippy::expect_used)]

    use super::*;

    /// The reference test key: bytes 0x00..0x0f.
    /// REF: TestCrypt.cpp : `testvectors` / `authcrypt` `rawkey`.
    const RAW_KEY: [u8; KEY_SIZE] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];

    fn cipher() -> Aes128 {
        Aes128::new(GenericArray::from_slice(&RAW_KEY))
    }

    // REF: TestCrypt.cpp : TestCrypt::testvectors (from draft-krovetz-ocb-00).
    #[test]
    fn ocb_test_vectors_from_krovetz_draft() {
        let cipher = cipher();

        // Empty plaintext: tag is the "blank tag".
        let (authentic, ciphertext, tag) = ocb_encrypt(&cipher, &[], &RAW_KEY, true);
        assert!(authentic);
        assert!(ciphertext.is_empty());
        const BLANK_TAG: Block = [
            0xBF, 0x31, 0x08, 0x13, 0x07, 0x73, 0xAD, 0x5E, 0xC7, 0x0E, 0xC6, 0x9E, 0x78, 0x75,
            0xA7, 0xB0,
        ];
        assert_eq!(tag, BLANK_TAG);

        // 40-byte plaintext 0x00..0x27.
        let source: Vec<u8> = (0u8..40).collect();
        let (authentic, ciphertext, tag) = ocb_encrypt(&cipher, &source, &RAW_KEY, true);
        assert!(authentic);
        const LONG_TAG: Block = [
            0x9D, 0xB0, 0xCD, 0xF8, 0x80, 0xF7, 0x3E, 0x3E, 0x10, 0xD4, 0xEB, 0x32, 0x17, 0x76,
            0x66, 0x88,
        ];
        const CRYPTED: [u8; 40] = [
            0xF7, 0x5D, 0x6B, 0xC8, 0xB4, 0xDC, 0x8D, 0x66, 0xB8, 0x36, 0xA2, 0xB0, 0x8B, 0x32,
            0xA6, 0x36, 0x9F, 0x1C, 0xD3, 0xC5, 0x22, 0x8D, 0x79, 0xFD, 0x6C, 0x26, 0x7F, 0x5F,
            0x6A, 0xA7, 0xB2, 0x31, 0xC7, 0xDF, 0xB9, 0xD5, 0x99, 0x51, 0xAE, 0x9C,
        ];
        assert_eq!(tag, LONG_TAG);
        assert_eq!(ciphertext, CRYPTED);
    }

    // REF: TestCrypt.cpp : TestCrypt::authcrypt.
    #[test]
    fn authcrypt_roundtrips_every_length_and_agrees_on_tag() {
        let nonce: Block = [
            0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22,
            0x11, 0x00,
        ];
        let cipher = cipher();
        for len in 0u32..128 {
            let src: Vec<u8> = (0..len).map(|i| (i + 1) as u8).collect();
            let (enc_ok, encrypted, enc_tag) = ocb_encrypt(&cipher, &src, &nonce, true);
            let (dec_ok, decrypted, dec_tag) = ocb_decrypt(&cipher, &encrypted, &nonce);
            assert!(enc_ok, "encrypt authentic at len {len}");
            assert!(dec_ok, "decrypt authentic at len {len}");
            assert_eq!(enc_tag, dec_tag, "tags agree at len {len}");
            assert_eq!(decrypted, src, "roundtrip at len {len}");
        }
    }

    // REF: TestCrypt.cpp : TestCrypt::xexstarAttack.
    #[test]
    fn xexstar_attack_is_detected() {
        let nonce: Block = [
            0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22,
            0x11, 0x00,
        ];
        let cipher = cipher();

        let mut src = vec![0u8; 2 * BLOCK_SIZE];
        // First block: length of the second block, in bits.
        src[BLOCK_SIZE - 1] = (BLOCK_SIZE * 8) as u8;
        // Second block: arbitrary.
        for b in src[BLOCK_SIZE..].iter_mut() {
            *b = 42;
        }

        // Without the mitigation, encryption reports the critical block.
        let (enc_ok, mut encrypted, mut enc_tag) = ocb_encrypt(&cipher, &src, &nonce, false);
        let failed_encrypt = !enc_ok;

        // Perform the forgery on the ciphertext.
        encrypted[BLOCK_SIZE - 1] ^= (BLOCK_SIZE * 8) as u8;
        for i in 0..BLOCK_SIZE {
            enc_tag[i] = src[BLOCK_SIZE + i] ^ encrypted[BLOCK_SIZE + i];
        }

        // Decrypt just the first block: detection must fire.
        let (dec_ok, _plain, dec_tag) = ocb_decrypt(&cipher, &encrypted[..BLOCK_SIZE], &nonce);
        let failed_decrypt = !dec_ok;

        // The forged tag matches (attack is correctly reproduced) ...
        assert_eq!(enc_tag, dec_tag);
        // ... and both sides flag it.
        assert!(failed_encrypt);
        assert!(failed_decrypt);

        // With the mitigation on, the same plaintext encrypts and decrypts
        // authentically, and the critical first block is altered (0 -> 1).
        let (enc_ok, encrypted, enc_tag) = ocb_encrypt(&cipher, &src, &nonce, true);
        let (dec_ok, decrypted, dec_tag) = ocb_decrypt(&cipher, &encrypted, &nonce);
        assert!(enc_ok);
        assert!(dec_ok);
        assert_eq!(enc_tag, dec_tag);
        assert_eq!(src[0], 0);
        assert_eq!(decrypted[0], 1);
    }

    // REF: TestCrypt.cpp : TestCrypt::tamper.
    #[test]
    fn tamper_with_any_bit_is_rejected() {
        let nonce: Block = [
            0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22,
            0x11, 0x00,
        ];
        let mut state = CryptState::new(&RAW_KEY, &nonce, &nonce);
        // "It was a funky funky town!" plus the trailing NUL, as in the reference.
        let mut message = b"It was a funky funky town!".to_vec();
        message.push(0);

        let mut encrypted = state.encrypt(&message).expect("encrypt");
        let body_len = message.len();
        for i in 0..body_len * 8 {
            encrypted[i / 8] ^= 1u8 << (i % 8);
            assert!(
                state.decrypt(&encrypted).is_none(),
                "flipped bit {i} must be rejected"
            );
            encrypted[i / 8] ^= 1u8 << (i % 8);
        }
        assert_eq!(state.decrypt(&encrypted).as_deref(), Some(&message[..]));
    }

    // REF: TestCrypt.cpp : TestCrypt::ivrecovery.
    #[test]
    fn iv_recovery_and_replay_rejection() {
        let mut enc = CryptState::new(&RAW_KEY, &[0u8; BLOCK_SIZE], &[0u8; BLOCK_SIZE]);
        // Force the encrypt IV, then align the decryptor to it.
        let forced = [0x55u8; BLOCK_SIZE];
        enc.set_encrypt_iv(&forced);
        let mut dec = CryptState::new(&RAW_KEY, &enc.encrypt_iv(), &enc.encrypt_iv());

        let secret = b"abcdefghi\0";

        let crypted = enc.encrypt(secret).expect("encrypt");
        assert_eq!(dec.decrypt(&crypted).as_deref(), Some(&secret[..]));
        // Refuses to reuse the same IV (replay).
        assert!(dec.decrypt(&crypted).is_none());

        // Recover from lost packets.
        let mut crypted = crypted;
        for _ in 0..16 {
            crypted = enc.encrypt(secret).expect("encrypt");
        }
        assert!(dec.decrypt(&crypted).is_some());

        // Wraparound: 15 packets per round, decrypt the last; each round loses 14.
        for _ in 0..128 {
            dec.lost = 0;
            let mut last = Vec::new();
            for _ in 0..15 {
                last = enc.encrypt(secret).expect("encrypt");
            }
            assert!(dec.decrypt(&last).is_some());
            assert_eq!(dec.lost, 14);
        }
        assert_eq!(enc.encrypt_iv(), dec.decrypt_iv());

        // Wrap too far: 257 packets ahead cannot be recovered.
        let mut far = Vec::new();
        for _ in 0..257 {
            far = enc.encrypt(secret).expect("encrypt");
        }
        assert!(dec.decrypt(&far).is_none());

        // Resync and continue.
        dec.set_decrypt_iv(&enc.encrypt_iv());
        let next = enc.encrypt(secret).expect("encrypt");
        assert!(dec.decrypt(&next).is_some());
    }

    // REF: TestCrypt.cpp : TestCrypt::reverserecovery (out-of-order and replay).
    #[test]
    fn reverse_recovery_within_window_then_replay_rejected() {
        let forced = [0x55u8; BLOCK_SIZE];
        let mut enc = CryptState::new(&RAW_KEY, &forced, &forced);
        enc.set_encrypt_iv(&forced);
        let mut dec = CryptState::new(&RAW_KEY, &enc.encrypt_iv(), &enc.encrypt_iv());

        let secret = b"abcdefghi\0";

        // Encrypt 128 packets, decrypt the most recent 30 in reverse order.
        let mut packets = Vec::new();
        for _ in 0..128 {
            packets.push(enc.encrypt(secret).expect("encrypt"));
        }
        for i in 0..30 {
            assert!(dec.decrypt(&packets[127 - i]).is_some(), "reverse {i}");
        }
        // Beyond the recovery window, older packets are rejected.
        for i in 30..128 {
            assert!(dec.decrypt(&packets[127 - i]).is_none(), "too old {i}");
        }
        // Replaying the already-accepted ones is rejected too.
        for i in 0..30 {
            assert!(dec.decrypt(&packets[127 - i]).is_none(), "replay {i}");
        }
    }
}
