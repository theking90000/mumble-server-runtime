//! Transport-level TCP and UDP relaying with capture.
//!
//! This module moves bytes and logs them. It never parses Mumble framing,
//! protobuf, or OCB2: decoding is a later phase.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Mutex;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, ServerConfig};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::capture::{Dir, RecordSink, Transport};

const TCP_BUFFER_SIZE: usize = 16 * 1024;
const UDP_BUFFER_SIZE: usize = 64 * 1024;

/// Accept client TLS connections, terminate them, reconnect to `upstream` as a
/// TLS client, and relay both directions in clear while logging every segment.
///
/// A failure on one connection is logged and that connection is abandoned; the
/// listener keeps serving.
pub async fn serve_tcp(
    listen: SocketAddr,
    upstream: SocketAddr,
    tls_name: String,
    server_cfg: Arc<ServerConfig>,
    client_cfg: Arc<ClientConfig>,
    sink: RecordSink,
) -> Result<()> {
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding TCP listener on {listen}"))?;
    eprintln!("TCP control listener on {listen} -> {upstream}");

    loop {
        let (stream, peer) = listener
            .accept()
            .await
            .context("accepting TCP connection")?;
        let server_cfg = Arc::clone(&server_cfg);
        let client_cfg = Arc::clone(&client_cfg);
        let tls_name = tls_name.clone();
        let sink = sink.clone();
        tokio::spawn(async move {
            if let Err(error) =
                handle_tcp_connection(stream, upstream, tls_name, server_cfg, client_cfg, sink)
                    .await
            {
                eprintln!("TCP connection from {peer} ended with error: {error:#}");
            }
        });
    }
}

async fn handle_tcp_connection(
    client_stream: TcpStream,
    upstream: SocketAddr,
    tls_name: String,
    server_cfg: Arc<ServerConfig>,
    client_cfg: Arc<ClientConfig>,
    sink: RecordSink,
) -> Result<()> {
    let acceptor = TlsAcceptor::from(server_cfg);
    let client_tls = acceptor
        .accept(client_stream)
        .await
        .context("terminating client TLS")?;

    let upstream_tcp = TcpStream::connect(upstream)
        .await
        .with_context(|| format!("connecting to upstream {upstream}"))?;
    let connector = TlsConnector::from(client_cfg);
    let server_name = ServerName::try_from(tls_name.clone())
        .with_context(|| format!("invalid TLS server name {tls_name}"))?;
    let server_tls = connector
        .connect(server_name, upstream_tcp)
        .await
        .context("connecting upstream TLS")?;

    let (mut client_read, mut client_write) = tokio::io::split(client_tls);
    let (mut server_read, mut server_write) = tokio::io::split(server_tls);

    let c2s_sink = sink.clone();
    let client_to_server = async move {
        pump(
            &mut client_read,
            &mut server_write,
            &c2s_sink,
            Dir::ClientToServer,
        )
        .await
    };
    let s2c_sink = sink.clone();
    let server_to_client = async move {
        pump(
            &mut server_read,
            &mut client_write,
            &s2c_sink,
            Dir::ServerToClient,
        )
        .await
    };

    // Both directions are independent; the first to finish or error tears down
    // the connection. tokio::try_join drops the other future, which is safe:
    // pump holds no state that must be flushed beyond each write_all + flush.
    tokio::try_join!(client_to_server, server_to_client)?;
    Ok(())
}

async fn pump<R, W>(reader: &mut R, writer: &mut W, sink: &RecordSink, dir: Dir) -> Result<()>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let mut buffer = vec![0u8; TCP_BUFFER_SIZE];
    loop {
        let n = reader
            .read(&mut buffer)
            .await
            .context("reading TCP stream")?;
        if n == 0 {
            break;
        }
        let chunk = &buffer[..n];
        sink.log(dir, Transport::Tcp, chunk);
        writer
            .write_all(chunk)
            .await
            .context("writing TCP stream")?;
        writer.flush().await.context("flushing TCP stream")?;
    }
    Ok(())
}

/// Relay UDP voice datagrams blindly. Each distinct client source address gets
/// its own dedicated upstream socket so Murmur can tell clients apart.
pub async fn serve_udp(listen: SocketAddr, upstream: SocketAddr, sink: RecordSink) -> Result<()> {
    let client_facing = Arc::new(
        UdpSocket::bind(listen)
            .await
            .with_context(|| format!("binding UDP socket on {listen}"))?,
    );
    eprintln!("UDP voice relay on {listen} -> {upstream}");

    let uplinks: Arc<Mutex<HashMap<SocketAddr, Arc<UdpSocket>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let mut buffer = vec![0u8; UDP_BUFFER_SIZE];
    loop {
        let (n, client_addr) = client_facing
            .recv_from(&mut buffer)
            .await
            .context("receiving UDP datagram")?;
        let datagram = &buffer[..n];

        let uplink = {
            let mut table = uplinks.lock().await;
            match table.get(&client_addr) {
                Some(existing) => Arc::clone(existing),
                None => {
                    let uplink = match new_uplink(upstream).await {
                        Ok(socket) => socket,
                        Err(error) => {
                            eprintln!("failed to create UDP uplink for {client_addr}: {error:#}");
                            continue;
                        }
                    };
                    table.insert(client_addr, Arc::clone(&uplink));
                    spawn_uplink_reader(
                        Arc::clone(&uplink),
                        Arc::clone(&client_facing),
                        client_addr,
                        sink.clone(),
                    );
                    uplink
                }
            }
        };

        sink.log(Dir::ClientToServer, Transport::Udp, datagram);
        if let Err(error) = uplink.send(datagram).await {
            eprintln!("failed to forward UDP datagram from {client_addr}: {error:#}");
        }
    }
}

async fn new_uplink(upstream: SocketAddr) -> Result<Arc<UdpSocket>> {
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .context("binding UDP uplink socket")?;
    socket
        .connect(upstream)
        .await
        .with_context(|| format!("connecting UDP uplink to {upstream}"))?;
    Ok(Arc::new(socket))
}

/// Spawn a detached reader for one upstream socket. It is intentionally
/// unowned: its lifetime is the recording session, it holds no resource that
/// needs orderly shutdown, and it dies when the process exits at Ctrl-C.
fn spawn_uplink_reader(
    uplink: Arc<UdpSocket>,
    client_facing: Arc<UdpSocket>,
    client_addr: SocketAddr,
    sink: RecordSink,
) {
    tokio::spawn(async move {
        let mut buffer = vec![0u8; UDP_BUFFER_SIZE];
        loop {
            let n = match uplink.recv(&mut buffer).await {
                Ok(n) => n,
                Err(error) => {
                    eprintln!("UDP uplink for {client_addr} closed: {error:#}");
                    break;
                }
            };
            let datagram = &buffer[..n];
            sink.log(Dir::ServerToClient, Transport::Udp, datagram);
            if let Err(error) = client_facing.send_to(datagram, client_addr).await {
                eprintln!("failed to return UDP datagram to {client_addr}: {error:#}");
                break;
            }
        }
    });
}
