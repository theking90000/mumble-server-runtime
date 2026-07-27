//! Binding the sockets and starting everything.
//!
//! The operational shape of the guide (10.2): build the runtime, install the
//! router, create the initial shards, then serve. Shards created later by a
//! flavor need nothing from here.

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::net::{TcpListener, UdpSocket};
use tokio_rustls::TlsAcceptor;

use crate::config::GatewayConfig;
use crate::connection;
use crate::router::ConnectionRouter;
use crate::runtime::{Runtime, RuntimeHandle};
use crate::tls::{self, Identity};
use crate::voice::VoicePlane;

/// A bound gateway, ready to serve.
///
/// Built before it runs so a composition binary can create its shards - and a
/// test can learn the port the operating system picked - between binding and
/// serving.
pub struct Gateway {
    runtime: Runtime,
    config: Arc<GatewayConfig>,
    listener: TcpListener,
    udp: Arc<UdpSocket>,
    acceptor: TlsAcceptor,
}

impl Gateway {
    /// Bind both planes and start the runtime.
    ///
    /// # Errors
    ///
    /// When either socket cannot be bound, or the TLS identity is unusable.
    pub async fn bind(config: GatewayConfig, identity: Identity) -> Result<Gateway> {
        tls::install_crypto_provider();
        let acceptor = TlsAcceptor::from(tls::server_config(identity)?);
        let (listener, udp) = bind_both(config.bind).await?;

        let mut config = config;
        config.bind = listener.local_addr().context("TCP local address")?;

        Ok(Gateway {
            runtime: Runtime::start(),
            config: Arc::new(config),
            listener,
            udp: Arc::new(udp),
            acceptor,
        })
    }

    /// The address both planes ended up on.
    #[must_use]
    pub fn address(&self) -> std::net::SocketAddr {
        self.config.bind
    }

    #[must_use]
    pub fn runtime(&self) -> RuntimeHandle {
        self.runtime.handle()
    }

    /// Accept connections until the listener fails.
    ///
    /// # Errors
    ///
    /// Only when the listener itself fails: one refused connection never ends
    /// the gateway.
    pub async fn serve<R: ConnectionRouter>(self, router: R) -> Result<()> {
        let router = Arc::new(router);
        let runtime = self.runtime.handle();
        let voice = Arc::new(VoicePlane::new(
            Arc::clone(&self.udp),
            Arc::clone(runtime.peers()),
            (*self.config).clone(),
        ));

        // The voice plane is owned by this future rather than detached: when
        // `serve` ends, the plane ends with it.
        let plane = {
            let voice = Arc::clone(&voice);
            tokio::spawn(async move {
                if let Err(error) = voice.run().await {
                    eprintln!("voxloom-gateway: the voice plane stopped: {error}");
                }
            })
        };

        let accepting = async {
            loop {
                let (tcp, from) = self.listener.accept().await.context("TCP accept")?;
                let acceptor = self.acceptor.clone();
                let runtime = runtime.clone();
                let router = Arc::clone(&router);
                let voice = Arc::clone(&voice);
                let udp = Arc::clone(&self.udp);
                let config = Arc::clone(&self.config);

                // One task per connection, and it owns the socket outright.
                // Detached on purpose: a connection outlives nothing but itself,
                // and its cleanup is in its own tail rather than in a joiner.
                tokio::spawn(async move {
                    if let Err(error) =
                        connection::serve(tcp, acceptor, runtime, router, voice, udp, config).await
                    {
                        eprintln!("voxloom-gateway: connection from {from} ended: {error:#}");
                    }
                });
            }
        };

        let outcome: Result<()> = accepting.await;
        plane.abort();
        outcome
    }
}

/// How many times an ephemeral bind retries before giving up.
///
/// Only ever reached when the operating system hands out a TCP port whose UDP
/// twin is already taken, which is uncommon and independent between attempts.
const EPHEMERAL_ATTEMPTS: u32 = 16;

/// Bind the control plane and the voice plane on the same port.
///
/// Mumble uses one number for both, so the two binds have to agree. With an
/// explicit port that is one call each. With an ephemeral port (`:0`) it is a
/// race: the kernel picks a free **TCP** port, which says nothing about UDP, and
/// binding the pair can genuinely fail. Retrying is the honest answer - each
/// attempt draws an independent number - and it is bounded so a machine with no
/// free pair reports that instead of spinning.
async fn bind_both(address: std::net::SocketAddr) -> Result<(TcpListener, UdpSocket)> {
    let mut attempts = if address.port() == 0 {
        EPHEMERAL_ATTEMPTS
    } else {
        1
    };

    loop {
        attempts = attempts.saturating_sub(1);
        let listener = TcpListener::bind(address)
            .await
            .with_context(|| format!("binding TCP {address}"))?;
        let bound = listener.local_addr().context("TCP local address")?;

        match UdpSocket::bind(bound).await {
            Ok(udp) => return Ok((listener, udp)),
            Err(error) if attempts > 0 => {
                // Dropping the listener releases the TCP port, so the next
                // attempt is free to draw a different one.
                drop(listener);
                eprintln!("voxloom-gateway: UDP {bound} was taken ({error}), trying another port");
            }
            Err(error) => {
                return Err(anyhow::Error::from(error))
                    .with_context(|| format!("binding UDP {bound}"));
            }
        }
    }
}

/// Bind, create shards, and serve, in one call.
///
/// The shape most composition binaries want. `build` runs after the sockets are
/// bound and before the first connection is accepted, which is the only window
/// in which "the initial shards exist before anyone can be routed to one" is
/// guaranteed.
///
/// # Errors
///
/// Whatever [`Gateway::bind`] or [`Gateway::serve`] report.
pub async fn serve<R: ConnectionRouter>(
    config: GatewayConfig,
    identity: Identity,
    build: impl FnOnce(&RuntimeHandle) -> R,
) -> Result<()> {
    let gateway = Gateway::bind(config, identity).await?;
    let router = build(&gateway.runtime());
    gateway.serve(router).await
}
