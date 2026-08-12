use std::fs::File;
use std::io::BufReader;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;
use mumble_server_runtime_gateway::tls::Identity;
use mumble_spaces_server::{ControllerConfig, MetricsOutput, RunningControllerServer};

#[derive(Debug, Parser)]
#[command(name = "mumble-spaces-server")]
#[command(about = "Spaces application for Mumble Server Runtime")]
struct Arguments {
    #[arg(long, default_value = "127.0.0.1:4000")]
    controller_bind: SocketAddr,
    #[arg(long, default_value = "0.0.0.0:64738")]
    mumble_bind: SocketAddr,
    #[arg(long, default_value_t = 30)]
    lease_seconds: u64,
    #[arg(long, default_value_t = 30)]
    empty_space_grace_seconds: u64,
    #[arg(long, default_value_t = 64)]
    max_sessions: usize,
    #[arg(long, default_value_t = 10_000)]
    max_participants: usize,
    #[arg(long, default_value_t = 5_000)]
    max_participants_per_session: usize,
    #[arg(long, default_value_t = 1_024)]
    max_spaces: usize,
    #[arg(long, default_value_t = 1_024)]
    max_observations_per_session: usize,
    #[arg(long, default_value_t = 1_024)]
    queue_capacity: usize,
    #[arg(long, default_value_t = 4_194_304)]
    grpc_max_frame_bytes: usize,
    #[arg(long, default_value_t = 100)]
    max_mumble_connections: u32,
    #[arg(long)]
    allow_unauthenticated_controller_network: bool,
    #[arg(long)]
    mumble_cert: Option<PathBuf>,
    #[arg(long)]
    mumble_key: Option<PathBuf>,
    #[arg(long)]
    dev_self_signed: bool,
    #[arg(long)]
    metrics_output: Option<PathBuf>,
    #[arg(long, default_value_t = 1)]
    metrics_interval_seconds: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let arguments = Arguments::parse();
    let identity = identity(&arguments)?;
    let config = ControllerConfig {
        controller_bind: arguments.controller_bind,
        mumble_bind: arguments.mumble_bind,
        lease_duration: Duration::from_secs(arguments.lease_seconds),
        empty_space_grace: Duration::from_secs(arguments.empty_space_grace_seconds),
        max_sessions: arguments.max_sessions,
        max_participants: arguments.max_participants,
        max_participants_per_session: arguments.max_participants_per_session,
        max_spaces: arguments.max_spaces,
        max_observations_per_session: arguments.max_observations_per_session,
        queue_capacity: arguments.queue_capacity,
        grpc_max_frame_bytes: arguments.grpc_max_frame_bytes,
        max_mumble_connections: arguments.max_mumble_connections,
        allow_unauthenticated_controller_network: arguments
            .allow_unauthenticated_controller_network,
    };
    let metrics = arguments.metrics_output.map(|path| MetricsOutput {
        path,
        interval: Duration::from_secs(arguments.metrics_interval_seconds),
    });
    let server = RunningControllerServer::start_with_metrics(config, identity, metrics)
        .await
        .context("starting the Mumble Spaces server")?;
    eprintln!(
        "mumble-spaces-server: Controller listening on {}, Mumble listening on {}",
        server.controller_address(),
        server.mumble_address()
    );
    tokio::signal::ctrl_c()
        .await
        .context("waiting for shutdown signal")?;
    server.shutdown().await;
    Ok(())
}

fn identity(arguments: &Arguments) -> Result<Identity> {
    match (
        arguments.dev_self_signed,
        arguments.mumble_cert.as_deref(),
        arguments.mumble_key.as_deref(),
    ) {
        (true, None, None) => Identity::self_signed(vec!["localhost".to_owned()])
            .context("generating the development Mumble certificate"),
        (false, Some(certificate), Some(key)) => load_identity(certificate, key),
        (true, _, _) => bail!("--dev-self-signed cannot be combined with certificate files"),
        (false, _, _) => {
            bail!("provide --mumble-cert and --mumble-key, or explicitly use --dev-self-signed")
        }
    }
}

fn load_identity(certificate_path: &Path, key_path: &Path) -> Result<Identity> {
    let certificate_file = File::open(certificate_path)
        .with_context(|| format!("opening Mumble certificate {}", certificate_path.display()))?;
    let mut certificate_reader = BufReader::new(certificate_file);
    let certificate = rustls_pemfile::certs(&mut certificate_reader)
        .next()
        .transpose()
        .context("decoding the Mumble certificate PEM")?
        .context("the Mumble certificate PEM contains no certificate")?;

    let key_file = File::open(key_path)
        .with_context(|| format!("opening Mumble private key {}", key_path.display()))?;
    let mut key_reader = BufReader::new(key_file);
    let key = rustls_pemfile::private_key(&mut key_reader)
        .context("decoding the Mumble private key PEM")?
        .context("the Mumble private key PEM contains no supported key")?;
    Ok(Identity {
        cert: certificate,
        key,
    })
}
