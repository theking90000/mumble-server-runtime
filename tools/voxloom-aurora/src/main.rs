//! Composition binary: the Aurora/Borealis reference flavor on the Voxloom
//! runtime.
//!
//! This is the only place in the tree that names both sides. The runtime knows
//! no realm and the flavor knows no wire format; compiling them together here
//! is what makes a runnable server, and swapping the flavor for another one is
//! a change to this file alone.
//!
//! REF: docs/voxloom-roadmap-agents-v0_1.md P7 T8
//! REF: docs/voxloom-specification-technique-v0.1.md 24.2

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use voxloom_flavor::ServerPresentation;
use voxloom_flavor_reference::ReferenceFlavor;
use voxloom_server::config::ServerConfig;
use voxloom_server::server::Server;
use voxloom_server::tls::{self, Identity};

/// A declarative Mumble-compatible voice runtime, composed with the
/// Aurora/Borealis reference flavor.
#[derive(Parser, Debug)]
#[command(name = "voxloom-aurora", version, about)]
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
        server_name: cli.name.clone(),
        welcome_text: cli.welcome.clone(),
        ..Default::default()
    };

    // What a client sees is the flavor's decision, so the presentation belongs
    // to the flavor; the runtime keeps only the limits it enforces itself.
    let flavor = Arc::new(ReferenceFlavor::new(
        cli.name,
        ServerPresentation {
            welcome_text: (!cli.welcome.is_empty()).then_some(cli.welcome),
            allow_html: config.allow_html,
            max_message_length: Some(config.message_length),
            recording_allowed: config.recording_allowed,
        },
    ));

    // A self-signed identity for local use. A real deployment supplies a stable
    // certificate so clients' per-server preferences survive restarts (§21.2).
    let identity =
        Identity::self_signed(vec!["localhost".to_string()]).context("building TLS identity")?;

    let server = Server::bind(config, flavor, identity, cli.tcp, udp_addr).await?;
    let tcp_addr = server.tcp_addr()?;
    let udp_bound = server.udp_addr()?;
    eprintln!("voxloom-aurora: listening on TCP {tcp_addr}, UDP {udp_bound}");

    server.serve_forever().await
}
