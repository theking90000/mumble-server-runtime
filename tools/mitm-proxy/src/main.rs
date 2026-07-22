//! CLI wiring for the Voxloom MITM proxy (Phase 2). All logic lives in the lib
//! (`voxloom_mitm_proxy`); this binary only parses arguments, sets up TLS, and
//! runs the control-plane relay until a fatal error or Ctrl-C.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use clap::Parser;
use voxloom_mitm_proxy::{serve, tls};

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
        "MITM proxy: {} -> {} (Ctrl-C to stop)",
        cli.listen, cli.upstream
    );

    let relay = serve(
        cli.listen,
        cli.upstream,
        cli.tls_name.clone(),
        server_cfg,
        client_cfg,
    );

    tokio::select! {
        // serve only returns on a fatal bind/accept error; Ctrl-C is the normal
        // stop path. Cancelling the relay future is safe: every in-flight frame
        // is fully written and flushed before the next read.
        result = relay => result.context("control relay stopped")?,
        result = tokio::signal::ctrl_c() => {
            result.context("waiting for Ctrl-C")?;
            eprintln!("\nCtrl-C received, stopping.");
        }
    }

    Ok(())
}
