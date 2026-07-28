//! CLI wiring for the Mumble Server Runtime MITM proxy (Phase 2). All logic lives in the lib
//! (`voxloom_mitm_proxy`); this binary only parses arguments, sets up TLS, and
//! runs the control-plane relay until a fatal error or Ctrl-C.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use clap::Parser;
use voxloom_mitm_proxy::{Registry, serve, serve_udp, tls};

#[derive(Parser)]
#[command(
    name = "voxloom-mitm-proxy",
    version,
    about = "Mumble MITM oracle: re-encrypts the voice plane through an independent OCB2 domain per side"
)]
struct Cli {
    /// Address the proxy listens on for the Mumble client (TCP control plane).
    #[arg(long, default_value = "0.0.0.0:64738")]
    listen: SocketAddr,
    /// Real Murmur server address to forward to.
    #[arg(long, default_value = "127.0.0.1:64739")]
    upstream: SocketAddr,
    /// TLS server name presented to the upstream connector.
    #[arg(long, default_value = "localhost")]
    tls_name: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    tls::install_crypto_provider();
    let server_cfg = tls::server_config()?;
    let client_cfg = tls::client_config();

    eprintln!(
        "MITM proxy: {} -> {} (TCP control + UDP voice, Ctrl-C to stop)",
        cli.listen, cli.upstream
    );

    // One registry bridges the two planes: the TCP relay publishes each session
    // under its client IP, the UDP relay looks it up to re-encrypt that client's
    // voice through the same cipher domains (and its mid-session re-keys).
    let registry = Registry::new();

    tokio::select! {
        // Each relay only returns on a fatal socket error; Ctrl-C is the normal
        // stop path. Cancelling a relay future is safe: the TCP side fully writes
        // and flushes every in-flight frame before the next read, and the UDP side
        // holds no half-sent datagram across an await.
        result = serve(
            cli.listen,
            cli.upstream,
            cli.tls_name.clone(),
            server_cfg,
            client_cfg,
            registry.clone(),
        ) => result.context("control relay stopped")?,
        result = serve_udp(cli.listen, cli.upstream, registry.clone()) => {
            result.context("UDP voice relay stopped")?;
        }
        result = tokio::signal::ctrl_c() => {
            result.context("waiting for Ctrl-C")?;
            eprintln!("\nCtrl-C received, stopping.");
        }
    }

    Ok(())
}
