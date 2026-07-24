//! CLI entry point for the minimal Voxloom server (Phase 3).
//!
//! Thin wiring only: parse the endpoint, build a self-signed identity (or load
//! one), bind and serve. All behaviour lives in the library.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use clap::Parser;
use voxloom_server::config::ServerConfig;
use voxloom_server::server::Server;
use voxloom_server::tls::{self, Identity};

/// A declarative Mumble-compatible voice runtime — minimal server (Phase 3).
#[derive(Parser, Debug)]
#[command(name = "voxloom-server", version, about)]
struct Cli {
    /// TCP/TLS control endpoint to listen on.
    #[arg(long, default_value = "0.0.0.0:64738")]
    tcp: SocketAddr,

    /// UDP voice endpoint to listen on. Defaults to the TCP port on 0.0.0.0.
    #[arg(long)]
    udp: Option<SocketAddr>,

    /// Server / root-channel name shown to clients.
    #[arg(long, default_value = "Voxloom")]
    name: String,

    /// Welcome text sent on connect.
    #[arg(long, default_value = "Welcome to Voxloom")]
    welcome: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    tls::install_crypto_provider();

    // UDP defaults to the same port as TCP, on all interfaces.
    let udp_addr = cli
        .udp
        .unwrap_or_else(|| SocketAddr::new(std::net::Ipv4Addr::UNSPECIFIED.into(), cli.tcp.port()));

    let config = ServerConfig {
        server_name: cli.name,
        welcome_text: cli.welcome,
        ..Default::default()
    };

    // A self-signed identity for local use. A real deployment supplies a stable
    // certificate so clients' per-server preferences survive restarts (§21.2).
    let identity =
        Identity::self_signed(vec!["localhost".to_string()]).context("building TLS identity")?;

    let server = Server::bind(config, identity, cli.tcp, udp_addr).await?;
    let tcp_addr = server.tcp_addr()?;
    let udp_bound = server.udp_addr()?;
    eprintln!("voxloom-server: listening on TCP {tcp_addr}, UDP {udp_bound}");

    server.serve_forever().await
}
