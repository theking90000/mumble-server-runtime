//! `corpus-decode`: replay a captured `.voxcap` session through the pure codec
//! and crypto, printing a readable, byte-complete transcript.
//!
//! Usage: `corpus-decode <path>...` where each path is either a `.voxcap` file
//! or a scenario directory containing `session.voxcap`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use voxloom_corpus_decode::{
    Decoded, Event, Transcript, Transport, control_type_name, decode_session, read_records,
};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        bail!("usage: corpus-decode <voxcap-or-scenario-dir>...");
    }

    let mut failures = 0usize;
    for arg in &args {
        let path = resolve(Path::new(arg));
        if let Err(error) = decode_and_print(&path) {
            eprintln!("error: {} : {error:#}", path.display());
            failures += 1;
        }
    }

    if failures > 0 {
        bail!("{failures} of {} input(s) failed to decode", args.len());
    }
    Ok(())
}

/// A scenario directory is addressed by its `session.voxcap`; a file is used
/// as-is.
fn resolve(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join("session.voxcap")
    } else {
        path.to_path_buf()
    }
}

fn decode_and_print(path: &Path) -> Result<()> {
    let records = read_records(path).with_context(|| format!("reading {}", path.display()))?;
    let transcript =
        decode_session(&records).with_context(|| format!("decoding {}", path.display()))?;

    println!("== {} ==", path.display());
    println!(
        "records={} tcp_frames={} udp_packets={} udp_rejected={}",
        records.len(),
        transcript.tcp_frames,
        transcript.udp_packets,
        transcript.udp_rejected
    );

    let base = records.first().map(|r| r.ts_micros).unwrap_or(0);
    for event in &transcript.events {
        print_event(event, base);
    }

    print_footer(&transcript);
    println!();
    Ok(())
}

fn print_event(event: &Event, base: i64) {
    let millis = (event.ts_micros - base) as f64 / 1000.0;
    let transport = match event.transport {
        Transport::Tcp => "tcp",
        Transport::Udp => "udp",
    };
    let detail = match &event.decoded {
        Decoded::Control(message) => {
            format!("{}  {:?}", control_type_name(message), message)
        }
        Decoded::LegacyConnectivityPing(ping) => {
            format!("legacy connectivity ping  {ping:?}")
        }
        Decoded::Udp(message) => format!("UDP(protobuf)  {message:?}"),
        Decoded::DecryptedLegacyUdp {
            kind,
            plaintext_len,
        } => {
            format!(
                "UDP(decrypted, legacy)  {} ({plaintext_len} plaintext bytes; ADR-0001: not decoded)",
                kind.label()
            )
        }
        Decoded::RejectedUdp { len } => {
            format!("UDP(rejected by OCB2)  {len} bytes (replay/late/tag; dropped as Murmur would)")
        }
    };
    println!(
        "  [{millis:9.3}ms] {} {transport}  {detail}",
        event.dir.label()
    );
}

fn print_footer(transcript: &Transcript) {
    if transcript.tcp_trailing_c2s == 0 && transcript.tcp_trailing_s2c == 0 {
        println!("  -- TCP streams fully framed (0 trailing bytes both directions)");
    } else {
        println!(
            "  -- WARNING: trailing unparsed TCP bytes: c2s={} s2c={}",
            transcript.tcp_trailing_c2s, transcript.tcp_trailing_s2c
        );
    }
    // A handful of OCB2 rejections is a normal drop tail; a large share means a
    // decode bug (e.g. a missed re-key), so make any nonzero count loud.
    if transcript.udp_rejected > 0 {
        println!(
            "  -- WARNING: {} of {} UDP packets rejected by OCB2 (investigate if not a small tail)",
            transcript.udp_rejected, transcript.udp_packets
        );
    }
}
