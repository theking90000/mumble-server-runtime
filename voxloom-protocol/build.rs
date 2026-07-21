//! Generate Rust types from the vendored Mumble `.proto` files (R1).
//!
//! The wire schema is never hand-transcribed: it is compiled from
//! `references/vendored/*.proto` at the pinned commit (`references/mumble.pin`).
//! `protox` parses the `.proto` in pure Rust, so no external `protoc` is needed.
//!
//! This runs outside the crate's `src/`, so it is exempt from the pure-crate IO
//! gate; the generated code lands in `OUT_DIR` and is `include!`d by `messages.rs`.

use std::path::PathBuf;

const PROTOS: &[&str] = &[
    "../references/vendored/Mumble.proto",
    "../references/vendored/MumbleUDP.proto",
];
const INCLUDES: &[&str] = &["../references/vendored"];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let file_descriptors = protox::compile(PROTOS, INCLUDES)?;

    let out_dir = PathBuf::from(std::env::var("OUT_DIR")?);
    prost_build::Config::new()
        .out_dir(&out_dir)
        .compile_fds(file_descriptors)?;

    for proto in PROTOS {
        println!("cargo:rerun-if-changed={proto}");
    }
    println!("cargo:rerun-if-changed=../references/mumble.pin");
    Ok(())
}
