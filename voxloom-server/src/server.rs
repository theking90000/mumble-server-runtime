//! Server bring-up: bind the TCP and UDP sockets, then run the accept loop and
//! the UDP voice plane.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::net::{TcpListener, UdpSocket};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use crate::config::ServerConfig;
use crate::connection;
use crate::state::SharedState;
use crate::tls::{self, Identity};
use crate::voice::VoicePlane;

/// A bound-but-not-yet-running server. Splitting `bind` from `run` lets tests
/// learn the actual (ephemeral) addresses before driving traffic.
pub struct Server {
    state: Arc<SharedState>,
    acceptor: TlsAcceptor,
    tcp: TcpListener,
    udp: Arc<UdpSocket>,
}

impl Server {
    /// Bind the control (TCP/TLS) and voice (UDP) sockets. Passing port 0 in an
    /// address binds an ephemeral port; read it back with [`Server::tcp_addr`] /
    /// [`Server::udp_addr`].
    pub async fn bind(
        config: ServerConfig,
        identity: Identity,
        tcp_addr: SocketAddr,
        udp_addr: SocketAddr,
    ) -> Result<Self> {
        let acceptor = TlsAcceptor::from(tls::server_config(identity)?);
        let tcp = TcpListener::bind(tcp_addr)
            .await
            .with_context(|| format!("binding TCP {tcp_addr}"))?;
        let udp = UdpSocket::bind(udp_addr)
            .await
            .with_context(|| format!("binding UDP {udp_addr}"))?;
        let state = SharedState::new(config);
        Ok(Self {
            state,
            acceptor,
            tcp,
            udp: Arc::new(udp),
        })
    }

    /// The actual TCP control address (resolved port if 0 was requested).
    pub fn tcp_addr(&self) -> Result<SocketAddr> {
        self.tcp.local_addr().context("reading TCP local_addr")
    }

    /// The actual UDP voice address.
    pub fn udp_addr(&self) -> Result<SocketAddr> {
        self.udp.local_addr().context("reading UDP local_addr")
    }

    /// Shared state, exposed for tests and observability.
    pub fn state(&self) -> Arc<SharedState> {
        Arc::clone(&self.state)
    }

    /// Run until the TCP listener errors: spawn the UDP voice plane, then accept
    /// connections, one detached task each.
    pub async fn serve_forever(self) -> Result<()> {
        // The voice plane runs for the whole server lifetime; detaching it is
        // deliberate — it is cancelled when the server task is dropped.
        let voice = VoicePlane::new(Arc::clone(&self.udp), Arc::clone(&self.state));
        tokio::spawn(async move {
            if let Err(error) = voice.run().await {
                eprintln!("voxloom-server: voice plane stopped: {error}");
            }
        });

        loop {
            let (tcp, peer) = self.tcp.accept().await.context("TCP accept")?;
            let acceptor = self.acceptor.clone();
            let state = Arc::clone(&self.state);
            // Detached on purpose: a connection's lifetime is its own; its state
            // is deregistered inside `serve` on every exit path.
            tokio::spawn(async move {
                if let Err(error) = connection::serve(tcp, acceptor, state).await {
                    eprintln!("voxloom-server: connection {peer} ended: {error}");
                }
            });
        }
    }

    /// Spawn [`Server::serve_forever`] on the current runtime and return a handle
    /// carrying the resolved addresses.
    pub fn spawn(self) -> Result<ServerHandle> {
        let tcp_addr = self.tcp_addr()?;
        let udp_addr = self.udp_addr()?;
        let join = tokio::spawn(self.serve_forever());
        Ok(ServerHandle {
            tcp_addr,
            udp_addr,
            join,
        })
    }
}

/// Handle to a running server: its addresses and the task it runs on. Dropping
/// the handle detaches; call [`ServerHandle::shutdown`] to stop it.
pub struct ServerHandle {
    pub tcp_addr: SocketAddr,
    pub udp_addr: SocketAddr,
    join: JoinHandle<Result<()>>,
}

impl ServerHandle {
    /// Abort the server task (accept loop and voice plane).
    pub fn shutdown(self) {
        self.join.abort();
    }
}
