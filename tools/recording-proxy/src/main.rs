//! Mumble Server Runtime recording proxy (Phase 0).
//!
//! Captures official Mumble client <-> Murmur server sessions into `.voxcap`
//! files: TLS control plane is terminated and logged in clear, UDP voice is
//! relayed blindly (never decrypted). See the module docs for the transport
//! details. This is a corpus-capture tool, not part of the runtime.

mod capture;
mod proxy;
mod tls;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::capture::{CaptureWriter, Dir, LiveStats, Record, Transport, now_micros, read_records};

#[derive(Parser)]
#[command(
    name = "voxloom-recording-proxy",
    version,
    about = "Mumble control/voice recording proxy for the Phase 0 corpus"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Record a live session by proxying between a Mumble client and Murmur.
    Record(RecordArgs),
    /// Dump a `.voxcap` file as a human-readable summary.
    Dump(DumpArgs),
}

#[derive(Parser)]
struct RecordArgs {
    /// Address the proxy listens on for the Mumble client (TCP and UDP).
    #[arg(long, default_value = "0.0.0.0:64738")]
    listen: SocketAddr,
    /// Real Murmur server address to forward to.
    #[arg(long, default_value = "127.0.0.1:64739")]
    upstream: SocketAddr,
    /// Output directory. Defaults to `capture-<unix_secs>`.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Scenario name recorded in meta.json.
    #[arg(long, default_value = "capture")]
    scenario: String,
    /// TLS server name presented to the upstream connector.
    #[arg(long, default_value = "localhost")]
    tls_name: String,
}

#[derive(Parser)]
struct DumpArgs {
    /// Path to the `.voxcap` file to read.
    file: PathBuf,
    /// Print a truncated hexdump of each record's data.
    #[arg(long)]
    hex: bool,
    /// Maximum number of data bytes to hexdump per record.
    #[arg(long, default_value_t = 64)]
    max_bytes: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Record(args) => run_record(args).await,
        Command::Dump(args) => run_dump(args),
    }
}

#[derive(Serialize)]
struct Meta {
    tool_version: String,
    scenario: String,
    listen: String,
    upstream: String,
    tls_name: String,
    started_unix_micros: i64,
    ended_unix_micros: i64,
    records_total: u64,
    records_tcp: u64,
    records_udp: u64,
    bytes_c2s_tcp: u64,
    bytes_s2c_tcp: u64,
    bytes_c2s_udp: u64,
    bytes_s2c_udp: u64,
}

async fn run_record(args: RecordArgs) -> Result<()> {
    tls::install_crypto_provider();
    let server_cfg = tls::server_config()?;
    let client_cfg = tls::client_config();

    let out_dir = match args.out {
        Some(dir) => dir,
        None => {
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            PathBuf::from(format!("capture-{secs}"))
        }
    };
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("creating output directory {}", out_dir.display()))?;
    let capture_path = out_dir.join("session.voxcap");

    let writer = CaptureWriter::create(&capture_path)?;
    let sink = writer.sink();
    let stats = writer.stats();
    let started_unix_micros = now_micros();

    eprintln!(
        "recording scenario '{}' to {} (Ctrl-C to stop)",
        args.scenario,
        capture_path.display()
    );

    let tcp = proxy::serve_tcp(
        args.listen,
        args.upstream,
        args.tls_name.clone(),
        server_cfg,
        client_cfg,
        sink.clone(),
    );
    let udp = proxy::serve_udp(args.listen, args.upstream, sink.clone());

    tokio::select! {
        // serve_tcp/serve_udp only return on a fatal bind/accept error; either
        // aborts the run. ctrl_c is the normal stop path. Cancelling the two
        // relay futures is safe: each in-flight segment is flushed before the
        // next await, so no partial record is left behind.
        result = tcp => {
            result.context("TCP relay stopped")?;
        }
        result = udp => {
            result.context("UDP relay stopped")?;
        }
        result = tokio::signal::ctrl_c() => {
            result.context("waiting for Ctrl-C")?;
            eprintln!("\nCtrl-C received, draining capture...");
        }
    }

    drain(&stats).await;

    let ended_unix_micros = now_micros();
    let meta = Meta {
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        scenario: args.scenario,
        listen: args.listen.to_string(),
        upstream: args.upstream.to_string(),
        tls_name: args.tls_name,
        started_unix_micros,
        ended_unix_micros,
        records_total: stats.written.load(Ordering::Relaxed),
        records_tcp: stats.recs_tcp.load(Ordering::Relaxed),
        records_udp: stats.recs_udp.load(Ordering::Relaxed),
        bytes_c2s_tcp: stats.bytes_c2s_tcp.load(Ordering::Relaxed),
        bytes_s2c_tcp: stats.bytes_s2c_tcp.load(Ordering::Relaxed),
        bytes_c2s_udp: stats.bytes_c2s_udp.load(Ordering::Relaxed),
        bytes_s2c_udp: stats.bytes_s2c_udp.load(Ordering::Relaxed),
    };
    let meta_path = out_dir.join("meta.json");
    let meta_json = serde_json::to_string_pretty(&meta).context("serializing meta.json")?;
    std::fs::write(&meta_path, meta_json)
        .with_context(|| format!("writing {}", meta_path.display()))?;
    eprintln!(
        "wrote {} records to {}, metadata in {}",
        meta.records_total,
        capture_path.display(),
        meta_path.display()
    );

    Ok(())
}

/// Wait for the writer thread to persist everything enqueued at stop time,
/// polling every 20 ms up to a 2 s ceiling.
async fn drain(stats: &LiveStats) {
    let target = stats.enqueued.load(Ordering::Relaxed);
    let deadline = 100; // 100 * 20 ms = 2 s ceiling.
    for _ in 0..deadline {
        if stats.written.load(Ordering::Relaxed) >= target {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn run_dump(args: DumpArgs) -> Result<()> {
    let records = read_records(&args.file)?;
    print_summary(&records);
    for (index, record) in records.iter().enumerate() {
        print_record(index, record, args.hex, args.max_bytes);
    }
    Ok(())
}

fn print_summary(records: &[Record]) {
    let mut bytes_c2s_tcp = 0u64;
    let mut bytes_s2c_tcp = 0u64;
    let mut bytes_c2s_udp = 0u64;
    let mut bytes_s2c_udp = 0u64;
    let mut recs_tcp = 0u64;
    let mut recs_udp = 0u64;

    for record in records {
        let len = record.data.len() as u64;
        match (record.dir, record.transport) {
            (Dir::ClientToServer, Transport::Tcp) => bytes_c2s_tcp += len,
            (Dir::ServerToClient, Transport::Tcp) => bytes_s2c_tcp += len,
            (Dir::ClientToServer, Transport::Udp) => bytes_c2s_udp += len,
            (Dir::ServerToClient, Transport::Udp) => bytes_s2c_udp += len,
        }
        match record.transport {
            Transport::Tcp => recs_tcp += 1,
            Transport::Udp => recs_udp += 1,
        }
    }

    println!(
        "records: {} (tcp={recs_tcp}, udp={recs_udp})",
        records.len()
    );
    println!("tcp bytes: c2s={bytes_c2s_tcp}, s2c={bytes_s2c_tcp}");
    println!("udp bytes: c2s={bytes_c2s_udp}, s2c={bytes_s2c_udp}");
    println!("---");
}

fn print_record(index: usize, record: &Record, hex: bool, max_bytes: usize) {
    let dir = match record.dir {
        Dir::ClientToServer => "c2s",
        Dir::ServerToClient => "s2c",
    };
    let transport = match record.transport {
        Transport::Tcp => "tcp",
        Transport::Udp => "udp",
    };
    println!(
        "[{index}] ts={} {dir} {transport} len={}",
        record.ts_micros,
        record.data.len()
    );
    if hex {
        let shown = record.data.len().min(max_bytes);
        let dump = hexdump(&record.data[..shown]);
        let suffix = if record.data.len() > shown {
            format!(" ... (+{} bytes)", record.data.len() - shown)
        } else {
            String::new()
        };
        println!("    {dump}{suffix}");
    }
}

fn hexdump(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 3);
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
