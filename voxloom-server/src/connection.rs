//! The async per-connection control task.
//!
//! One task per TCP connection: terminate TLS, send the server `Version`, wait
//! for the client's `Authenticate`, emit the ordered handshake ([`handshake`]),
//! then service the connection — pings, TCP-tunnelled loopback audio, and
//! presence updates pushed from other connections — until it closes.

use std::sync::Arc;

use anyhow::{Context, Result};
use ring::rand::SystemRandom;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;
use voxloom_protocol::messages::tcp;
use voxloom_protocol::{
    ControlMessage, UdpMessage, decode_frame, decode_udp, encode_frame, encode_udp, parse_frame,
};

use crate::handshake::{self, OtherUser};
use crate::state::{SessionId, SharedState, UserEntry};
use crate::voice::{self, LOOPBACK_TARGET};

/// Serve one accepted TCP connection to completion. Errors (TLS failure, a
/// malformed frame, a dropped socket) end the connection cleanly: the user is
/// removed and its departure broadcast.
pub async fn serve(tcp: TcpStream, acceptor: TlsAcceptor, state: Arc<SharedState>) -> Result<()> {
    // A fresh randomness handle per connection; `SystemRandom` is a cheap ZST.
    let rng = SystemRandom::new();
    // Nagle off: control latency matters more than coalescing small frames.
    let _ignored = tcp.set_nodelay(true);
    let tls = acceptor.accept(tcp).await.context("TLS handshake")?;
    let (read_half, write_half) = tokio::io::split(tls);
    let mut reader = FrameReader::new(read_half);
    let mut writer = write_half;

    // The server announces its version immediately after TLS completes
    // (REF Server.cpp::encrypted), before the client authenticates.
    write_message(&mut writer, &handshake::server_version(state.config())).await?;

    // Wait for Authenticate. The client's Version (and anything else) before it
    // is accepted and ignored here.
    let authenticate = match wait_for_authenticate(&mut reader).await? {
        Some(auth) => auth,
        None => return Ok(()), // client left before authenticating
    };

    // Stub authentication (spec §10.2 token flow is P8): any username is accepted,
    // the password/token is treated as an opaque credential and not validated.
    // The client-proposed username is only a display suggestion (§10.5).
    let name = sanitize_username(authenticate.username.as_deref().unwrap_or("Guest"));

    let session = state.allocate_session();
    let (crypt_setup, crypt_state) = voice::generate_crypt_setup(&rng)?;

    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<ControlMessage>();
    let user = Arc::new(UserEntry {
        session,
        name: name.clone(),
        channel_id: crate::state::ROOT_CHANNEL_ID,
        outbound: outbound_tx,
        crypto: std::sync::Mutex::new(Some(crypt_state)),
        udp_addr: std::sync::Mutex::new(None),
    });

    // Register atomically and learn who was already present.
    let others = state.insert_user(Arc::clone(&user));
    let other_views: Vec<OtherUser> = others
        .iter()
        .map(|u| OtherUser {
            session: u.session,
            name: u.name.clone(),
            channel_id: u.channel_id,
        })
        .collect();

    // Emit the handshake.
    let messages = handshake::build_handshake(
        state.config(),
        state.channels(),
        session,
        &name,
        user.channel_id,
        crypt_setup,
        &other_views,
    );
    for message in &messages {
        write_message(&mut writer, message).await?;
    }

    // Announce the newcomer to everyone already connected (§17 presence).
    let announce = ControlMessage::UserState(tcp::UserState {
        session: Some(session),
        name: Some(name.clone()),
        channel_id: Some(user.channel_id),
        ..Default::default()
    });
    for other in &others {
        let _ignored = other.outbound.send(announce.clone());
    }

    // Service the connection. On any exit path we deregister and announce the
    // departure, so this is wrapped to run the cleanup unconditionally.
    let result = service_loop(&mut reader, &mut writer, &mut outbound_rx, session).await;

    state.remove_user(session);
    let departure = ControlMessage::UserRemove(tcp::UserRemove {
        session,
        ..Default::default()
    });
    for remaining in state.users() {
        let _ignored = remaining.outbound.send(departure.clone());
    }

    result
}

/// The steady-state loop: interleave reading client frames with writing frames
/// pushed by other connections (presence) onto this connection's queue.
async fn service_loop(
    reader: &mut FrameReader,
    writer: &mut WriteHalf<TlsStream<TcpStream>>,
    outbound_rx: &mut mpsc::UnboundedReceiver<ControlMessage>,
    session: SessionId,
) -> Result<()> {
    loop {
        tokio::select! {
            // Cancellation-safety: `FrameReader::next` only reads into an owned
            // buffer and never leaves a half-consumed frame across an await
            // boundary, so dropping it on the other branch loses nothing.
            incoming = reader.next() => {
                match incoming? {
                    None => return Ok(()), // client closed the connection
                    Some(message) => {
                        for reply in handle_control(message, session) {
                            write_message(writer, &reply).await?;
                        }
                    }
                }
            }
            // Cancellation-safety: `recv` is cancel-safe; a message is only taken
            // from the channel when this branch is selected.
            queued = outbound_rx.recv() => {
                match queued {
                    Some(message) => write_message(writer, &message).await?,
                    None => return Ok(()), // no senders left (cannot happen while we hold the user)
                }
            }
        }
    }
}

/// Read frames until the client's `Authenticate` arrives (or it disconnects).
async fn wait_for_authenticate(reader: &mut FrameReader) -> Result<Option<tcp::Authenticate>> {
    loop {
        match reader.next().await? {
            None => return Ok(None),
            Some(ControlMessage::Authenticate(auth)) => return Ok(Some(auth)),
            // Version and any other pre-auth chatter is accepted and ignored.
            Some(_) => continue,
        }
    }
}

/// Handle one control message from the client, returning replies to send back on
/// the same connection. Every branch ends in an explicit outcome (L4): a reply,
/// or an intentional (logged) drop for intents P3 does not support.
fn handle_control(message: ControlMessage, session: SessionId) -> Vec<ControlMessage> {
    match message {
        // TCP ping: echo the timestamp so the client can measure RTT (§16.3).
        ControlMessage::Ping(ping) => vec![ControlMessage::Ping(tcp::Ping {
            timestamp: ping.timestamp,
            ..Default::default()
        })],

        // Audio tunnelled over TCP (UDP fallback, §15.6): the payload is the
        // plaintext UDP packet. Reflect loopback (target 31) back over the tunnel.
        ControlMessage::UdpTunnel(raw) => match tunnel_loopback(&raw, session) {
            Some(reply) => vec![reply],
            None => Vec::new(),
        },

        // Everything else a client may send is, in P3, either a self-state change
        // we do not yet reflect or an action we refuse by default (spec §16.5-16.8,
        // §16.11...). Drop it explicitly rather than acting on it (fail closed).
        other => {
            drop_unsupported(&other, session);
            Vec::new()
        }
    }
}

/// Reflect a TCP-tunnelled loopback audio packet, or `None` if it is not
/// loopback (target 31) or is malformed.
fn tunnel_loopback(raw: &[u8], session: SessionId) -> Option<ControlMessage> {
    let audio = match decode_udp(raw) {
        Ok(UdpMessage::Audio(audio)) => audio,
        _ => return None,
    };
    let target = match audio.header {
        Some(voxloom_protocol::messages::udp::audio::Header::Target(target)) => target,
        _ => return None,
    };
    if target != LOOPBACK_TARGET {
        return None;
    }
    let reflected = voice::server_audio_for_tunnel(&audio, session);
    Some(ControlMessage::UdpTunnel(encode_udp(&UdpMessage::Audio(
        reflected,
    ))))
}

/// Log an unsupported client intent. Named so the drop is auditable rather than
/// silent (R6).
fn drop_unsupported(message: &ControlMessage, session: SessionId) {
    let kind = match message {
        ControlMessage::UserState(_) => "UserState",
        ControlMessage::ChannelState(_) => "ChannelState",
        ControlMessage::ChannelRemove(_) => "ChannelRemove",
        ControlMessage::UserRemove(_) => "UserRemove",
        ControlMessage::TextMessage(_) => "TextMessage",
        ControlMessage::Acl(_) => "ACL",
        ControlMessage::VoiceTarget(_) => "VoiceTarget",
        ControlMessage::CryptSetup(_) => "CryptSetup(resync)",
        ControlMessage::PermissionQuery(_) => "PermissionQuery",
        _ => "unsupported message",
    };
    eprintln!("voxloom-server: session {session}: refusing unsupported {kind} (P3)");
}

/// Normalise a client-proposed username: trim, collapse to non-empty, cap length.
/// The server is authoritative over the display name (spec §10.5).
fn sanitize_username(raw: &str) -> String {
    let trimmed = raw.trim();
    let name = if trimmed.is_empty() { "Guest" } else { trimmed };
    name.chars().take(64).collect()
}

/// Frame the message and write it, flushing so it is not stuck in a buffer.
async fn write_message(
    writer: &mut WriteHalf<TlsStream<TcpStream>>,
    message: &ControlMessage,
) -> Result<()> {
    let mut framed = Vec::new();
    encode_frame(message, &mut framed).context("encoding control frame")?;
    writer.write_all(&framed).await.context("TCP write")?;
    writer.flush().await.context("TCP flush")?;
    Ok(())
}

/// Incremental frame reader over the TLS read half.
struct FrameReader {
    read: ReadHalf<TlsStream<TcpStream>>,
    buffer: Vec<u8>,
}

impl FrameReader {
    fn new(read: ReadHalf<TlsStream<TcpStream>>) -> Self {
        Self {
            read,
            buffer: Vec::with_capacity(4096),
        }
    }

    /// Read the next complete control message, or `None` at a clean EOF on a
    /// frame boundary. Buffers partial reads and reassembles across TLS records.
    async fn next(&mut self) -> Result<Option<ControlMessage>> {
        loop {
            // Try to parse a complete frame out of what we already have.
            if let Some((message, consumed)) = self.try_parse()? {
                self.buffer.drain(..consumed);
                return Ok(Some(message));
            }

            // Need more bytes.
            let mut chunk = [0u8; 4096];
            let read = self.read.read(&mut chunk).await.context("TCP read")?;
            if read == 0 {
                if self.buffer.is_empty() {
                    return Ok(None); // clean close on a frame boundary
                }
                anyhow::bail!(
                    "connection closed mid-frame ({} bytes buffered)",
                    self.buffer.len()
                );
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }

    /// Parse one frame from the buffer without consuming it, returning the decoded
    /// message and the number of bytes it occupies.
    fn try_parse(&self) -> Result<Option<(ControlMessage, usize)>> {
        match parse_frame(&self.buffer).context("framing")? {
            Some(frame) => {
                let consumed = frame.total_len();
                let message = decode_frame(&frame).context("decoding control message")?;
                Ok(Some((message, consumed)))
            }
            None => Ok(None),
        }
    }
}
