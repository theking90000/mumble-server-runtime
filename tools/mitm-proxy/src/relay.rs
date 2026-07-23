//! Framed TCP control-plane relay for the MITM proxy.
//!
//! Unlike the Phase 0 recording proxy, this relay is not blind: it reassembles
//! Mumble frames, decodes each to a typed [`ControlMessage`], asks the per-
//! connection [`Session`] what to do, then re-encodes and forwards. Decoding and
//! re-encoding every live message is deliberate: it exercises the Phase 1 codec
//! against real traffic and is the hook the crypto rewrite lives on.
//!
//! A hard framing or decode error, or a rejected `CryptSetup`, tears the
//! connection down (fail closed, L4). The UDP voice plane it feeds lives in
//! [`crate::udp_relay`]; this relay publishes each session so that plane can find
//! its cipher domains.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{Context, Result, anyhow};
use ring::rand::{SecureRandom, SystemRandom};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex as TokioMutex;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, ServerConfig};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use voxloom_crypto::{BLOCK_SIZE, KEY_SIZE};
use voxloom_protocol::{ControlMessage, decode_frame, encode_frame, parse_frame};

use crate::session::{Action, ProxySecrets, Session};
use crate::udp_relay::Registry;

const TCP_BUFFER_SIZE: usize = 16 * 1024;

/// Which side a message came from, so the session applies the right domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Client,
    Server,
}

/// Generate fresh per-session OCB2 secrets for the proxy's client-facing domain.
pub fn random_secrets() -> Result<ProxySecrets> {
    let rng = SystemRandom::new();
    let mut key = [0u8; KEY_SIZE];
    let mut encrypt_iv = [0u8; BLOCK_SIZE];
    let mut decrypt_iv = [0u8; BLOCK_SIZE];
    for buffer in [&mut key[..], &mut encrypt_iv[..], &mut decrypt_iv[..]] {
        rng.fill(buffer)
            .map_err(|_| anyhow!("system RNG failed to produce proxy secrets"))?;
    }
    Ok(ProxySecrets::new(key, encrypt_iv, decrypt_iv))
}

/// Accept client TLS connections, MITM each to `upstream`, and relay the control
/// plane framed. One failed connection is logged; the listener keeps serving.
pub async fn serve(
    listen: SocketAddr,
    upstream: SocketAddr,
    tls_name: String,
    server_cfg: Arc<ServerConfig>,
    client_cfg: Arc<ClientConfig>,
    registry: Registry,
) -> Result<()> {
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding TCP listener on {listen}"))?;
    eprintln!("MITM control listener on {listen} -> {upstream}");

    loop {
        let (stream, peer) = listener
            .accept()
            .await
            .context("accepting TCP connection")?;
        let server_cfg = Arc::clone(&server_cfg);
        let client_cfg = Arc::clone(&client_cfg);
        let tls_name = tls_name.clone();
        let registry = registry.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(
                stream, peer, upstream, tls_name, server_cfg, client_cfg, registry,
            )
            .await
            {
                eprintln!("connection from {peer} ended with error: {error:#}");
            }
        });
    }
}

async fn handle_connection(
    client_stream: TcpStream,
    peer: SocketAddr,
    upstream: SocketAddr,
    tls_name: String,
    server_cfg: Arc<ServerConfig>,
    client_cfg: Arc<ClientConfig>,
    registry: Registry,
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

    let (client_read, client_write) = tokio::io::split(client_tls);
    let (server_read, server_write) = tokio::io::split(server_tls);
    let client_write = Arc::new(TokioMutex::new(client_write));
    let server_write = Arc::new(TokioMutex::new(server_write));
    let session = Arc::new(StdMutex::new(Session::new(random_secrets()?)));

    // Publish this session under the client's IP so the UDP relay can correlate
    // its voice datagrams to these cipher domains. The guard deregisters when the
    // connection ends, so a closed session never binds a later datagram.
    let _registration = registry.register(peer.ip(), Arc::clone(&session));

    // client -> server: forward toward the server, replies go back to the client.
    let c2s = pump_control(
        client_read,
        Arc::clone(&server_write),
        Arc::clone(&client_write),
        Arc::clone(&session),
        Origin::Client,
    );
    // server -> client: forward toward the client, replies go back to the server.
    let s2c = pump_control(
        server_read,
        Arc::clone(&client_write),
        Arc::clone(&server_write),
        Arc::clone(&session),
        Origin::Server,
    );

    // First direction to finish or error tears the connection down. Dropping the
    // other future is safe: each frame is fully written and flushed before the
    // next read, so no half-written frame is left behind.
    tokio::try_join!(c2s, s2c)?;
    Ok(())
}

/// Read frames from `reader`, decode each, run it through `session`, and act on
/// the result: forward toward the far side, reply toward the sender, or drop.
pub async fn pump_control<R, Wf, Wb>(
    mut reader: R,
    forward: Arc<TokioMutex<Wf>>,
    back: Arc<TokioMutex<Wb>>,
    session: Arc<StdMutex<Session>>,
    origin: Origin,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    Wf: AsyncWrite + Unpin,
    Wb: AsyncWrite + Unpin,
{
    let mut pending = Vec::new();
    let mut chunk = vec![0u8; TCP_BUFFER_SIZE];
    loop {
        let read = reader
            .read(&mut chunk)
            .await
            .context("reading control TLS")?;
        if read == 0 {
            break;
        }
        pending.extend_from_slice(&chunk[..read]);

        // Drain every complete frame the buffer now holds.
        loop {
            let (message, consumed) = match parse_frame(&pending).context("framing control")? {
                Some(frame) => (
                    decode_frame(&frame).context("decoding control")?,
                    frame.total_len(),
                ),
                None => break,
            };
            pending.drain(..consumed);

            let action = {
                let mut guard = session
                    .lock()
                    .map_err(|_| anyhow!("session state lock poisoned"))?;
                match origin {
                    Origin::Client => guard.process_from_client(message),
                    Origin::Server => guard.process_from_server(message),
                }
            }
            .context("processing control message")?;

            match action {
                Action::Forward(message) => write_message(&forward, &message).await?,
                Action::ReplyToSender(message) => write_message(&back, &message).await?,
                Action::Drop => {}
            }
        }
    }
    Ok(())
}

/// Frame `message` and write it to `writer`, flushing before returning.
async fn write_message<W>(writer: &Arc<TokioMutex<W>>, message: &ControlMessage) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut framed = Vec::new();
    encode_frame(message, &mut framed).context("encoding control frame")?;
    let mut guard = writer.lock().await;
    guard
        .write_all(&framed)
        .await
        .context("writing control TLS")?;
    guard.flush().await.context("flushing control TLS")?;
    Ok(())
}
