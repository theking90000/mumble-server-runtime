//! Cryptography for the Mumble voice plane: OCB2-AES128, IV management and
//! replay protection. Pure Rust, no IO, no runtime, no `unsafe` (gates R4).
//!
//! The algorithm and Mumble's specific mitigations are ported line-for-line from
//! the vendored reference (R1): `runtime/references/vendored/ocb2-vectors/CryptStateOCB2.cpp`
//! at `runtime/references/mumble.pin`, and validated against the vectors in the sibling
//! `TestCrypt.cpp`. Nothing here is recalled from memory of the OCB2 spec.
#![forbid(unsafe_code)]

pub mod ocb2;

pub use ocb2::{BLOCK_SIZE, CryptState, KEY_SIZE, ocb_decrypt, ocb_encrypt};
