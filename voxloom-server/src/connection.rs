//! The async per-connection control task.
//!
//! One task per TCP connection: terminate TLS, send the server `Version`, wait
//! for the client's `Authenticate`, emit the ordered handshake ([`handshake`]),
//! then service the connection — pings, TCP-tunnelled loopback audio, and
//! presence updates pushed from other connections — until it closes.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use ring::rand::SystemRandom;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;
use voxloom_protocol::messages::tcp;
use voxloom_protocol::{
    ControlMessage, UdpMessage, decode_frame, decode_udp, encode_frame, parse_frame,
};

use crate::handshake::{self, OtherUser};
use crate::limits;
use crate::outbound::{ControlAdmission, OutboundQueue};
use crate::state::{SessionId, SharedState, UserEntry};
use crate::voice;

/// Serve one accepted TCP connection to completion. Errors (TLS failure, a
/// malformed frame, a dropped socket) end the connection cleanly: the user is
/// removed and its departure broadcast.
///
/// The UDP socket is needed even on this path: a client speaking through the TCP
/// tunnel may have listeners who are on UDP, and they must still be reached.
pub async fn serve(
    tcp: TcpStream,
    acceptor: TlsAcceptor,
    state: Arc<SharedState>,
    udp: Arc<UdpSocket>,
) -> Result<()> {
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

    let (outbound, mut outbound_rx) = OutboundQueue::new(session);
    let user = Arc::new(UserEntry {
        session,
        name: name.clone(),
        channel_id: crate::state::ROOT_CHANNEL_ID,
        outbound,
        crypto: std::sync::Mutex::new(Some(crypt_state)),
        udp_addr: std::sync::Mutex::new(None),
        // Optimistic, like the real server: assume UDP until the client tells
        // us otherwise by tunnelling audio (REF `ServerUser.cpp`).
        udp_mode: std::sync::atomic::AtomicBool::new(true),
        voice_budget: std::sync::Mutex::new(crate::limits::VoiceBudget::new(Instant::now())),
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
        match other.outbound.push_control(announce.clone()) {
            ControlAdmission::Accepted => {}
            // That peer is too far behind to be told about the newcomer. It has
            // been marked for teardown by its own queue, and its task will act
            // on that; skipping it silently here would leave it permanently
            // blind to this user.
            ControlAdmission::Refused => {}
        }
    }

    // Service the connection. On any exit path we deregister and announce the
    // departure, so this is wrapped to run the cleanup unconditionally.
    let result = service_loop(
        &mut reader,
        &mut writer,
        &mut outbound_rx,
        &user,
        &state,
        &udp,
    )
    .await;

    state.remove_user(session);
    let departure = ControlMessage::UserRemove(tcp::UserRemove {
        session,
        ..Default::default()
    });
    for remaining in state.users() {
        match remaining.outbound.push_control(departure.clone()) {
            ControlAdmission::Accepted => {}
            // Same reasoning as the arrival broadcast: a peer that cannot take
            // the departure would keep a ghost user forever, so its queue has
            // already marked it for teardown.
            ControlAdmission::Refused => {}
        }
    }

    result
}

/// The steady-state loop: interleave reading client frames with writing frames
/// pushed by other connections (presence) onto this connection's queue.
async fn service_loop(
    reader: &mut FrameReader,
    writer: &mut WriteHalf<TlsStream<TcpStream>>,
    outbound_rx: &mut mpsc::Receiver<ControlMessage>,
    user: &UserEntry,
    state: &SharedState,
    udp: &UdpSocket,
) -> Result<()> {
    loop {
        // A refused control message means this client is too slow to keep a
        // correct view, so the connection ends and it can rebuild one by
        // reconnecting (the shape ADR-009 will take in Phase 6).
        //
        // Polling here rather than being woken is sound: the flag is only ever
        // set when the queue is full, which means there are `CAPACITY` messages
        // waiting for us, so the queued-message branch below fires immediately
        // and brings us straight back to this check.
        if user.outbound.must_close() {
            anyhow::bail!(
                "session {}: output queue overflowed, closing rather than diverging",
                user.session
            );
        }

        tokio::select! {
            // Cancellation-safety: `FrameReader::next` only reads into an owned
            // buffer and never leaves a half-consumed frame across an await
            // boundary, so dropping it on the other branch loses nothing.
            incoming = reader.next() => {
                match incoming? {
                    None => return Ok(()), // client closed the connection
                    // Tunnelled audio is routed like any other voice packet, so
                    // its recipients may well be on UDP.
                    Some(ControlMessage::UdpTunnel(raw)) => {
                        for (datagram, addr) in tunnel_audio(state, user, &raw) {
                            if let Err(error) = udp.send_to(&datagram, addr).await {
                                // One failed send never ends the connection.
                                eprintln!("voxloom-server: UDP send to {addr} failed: {error}");
                            }
                        }
                    }
                    Some(message) => {
                        for reply in handle_control(message, user) {
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
fn handle_control(message: ControlMessage, user: &UserEntry) -> Vec<ControlMessage> {
    let session = user.session;
    match message {
        // TCP ping: echo the timestamp so the client can measure RTT (§16.3),
        // plus our own crypt counters.
        ControlMessage::Ping(ping) => vec![ControlMessage::Ping(ping_reply(&ping, user))],

        // Everything else a client may send is, in P3, either a self-state change
        // we do not yet reflect or an action we refuse by default (spec §16.5-16.8,
        // §16.11...). Drop it explicitly rather than acting on it (fail closed).
        other => {
            drop_unsupported(&other, session);
            Vec::new()
        }
    }
}

/// Build the reply to a TCP `Ping`: the echoed timestamp plus the OCB2 counters
/// for the datagrams this user has sent us.
///
/// Reporting `good` is not optional bookkeeping: the client reads it as
/// `uiRemoteGood` and, if it is still zero 20 seconds into the session, decides
/// its UDP never reaches us and permanently falls back to the TCP tunnel — even
/// while the UDP plane is working in both directions.
///
/// REF: references/mumble/src/murmur/Messages.cpp : `Server::msgPing` — clears
///   the request and answers with timestamp, `uiGood`, `uiLate`, `uiLost`,
///   `uiResync`.
/// REF: references/mumble/src/mumble/ServerHandler.cpp : `ServerHandler::message`,
///   `TCPMessageType::Ping` — `(uiRemoteGood == 0 || uiGood == 0) && bUdp &&
///   elapsed > 20000000` disables UDP mode.
fn ping_reply(request: &tcp::Ping, user: &UserEntry) -> tcp::Ping {
    let (good, late, lost) = user.crypt_counters();
    tcp::Ping {
        timestamp: request.timestamp,
        good: Some(good),
        late: Some(late),
        lost: Some(lost),
        // Nonce resync is refused in P3 (see `drop_unsupported`), so zero is the
        // true count rather than a placeholder.
        resync: Some(0),
        ..Default::default()
    }
}

/// Route a TCP-tunnelled voice packet (the UDP fallback of spec 15.6).
///
/// The payload is a plaintext UDP packet, so once decoded it goes through the
/// very same routing as a datagram. Returned datagrams are for recipients that
/// are themselves on UDP; recipients on the tunnel are queued inside
/// [`crate::routing::deliver_audio`], including this sender's own loopback.
fn tunnel_audio(state: &SharedState, user: &UserEntry, raw: &[u8]) -> Vec<(Vec<u8>, SocketAddr)> {
    // The client is telling us its UDP does not work, so its audio goes back
    // over the tunnel until a datagram from it reaches us again.
    // REF: references/mumble/src/murmur/Server.cpp : the `UDPTunnel` branch sets
    //   `u->aiUdpFlag = 0`.
    user.set_udp_mode(false);

    if !limits::is_acceptable_size(raw.len()) {
        eprintln!(
            "voxloom-server: session {}: dropping {}-byte tunnelled packet (outside the accepted size band)",
            user.session,
            raw.len()
        );
        return Vec::new();
    }

    match decode_udp(raw) {
        Ok(UdpMessage::Audio(audio)) => {
            voice::route_client_audio(state, user, &audio, Instant::now())
        }
        Ok(UdpMessage::Ping(_)) => {
            // Connectivity pings belong on the UDP socket; one arriving here
            // measures nothing useful, so it is refused rather than answered.
            eprintln!(
                "voxloom-server: session {}: refusing a ping through the TCP tunnel",
                user.session
            );
            Vec::new()
        }
        Err(error) => {
            eprintln!(
                "voxloom-server: session {}: bad tunnelled envelope: {error}",
                user.session
            );
            Vec::new()
        }
    }
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
