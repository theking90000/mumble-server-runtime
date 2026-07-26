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
use voxloom_session::{EmittedStep, InboundCommand, wire_permissions};

use crate::handshake;
use crate::limits;
use crate::outbound::OutboundQueue;
use crate::projection;
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
    let (name, realm) =
        projection::scenario_identity(authenticate.username.as_deref().unwrap_or("Guest"));

    let session = state.allocate_session();
    let (crypt_setup, crypt_state) = voice::generate_crypt_setup(&rng)?;

    let (outbound, mut outbound_rx) = OutboundQueue::new(session);
    let user = Arc::new(UserEntry::new(
        session,
        name.clone(),
        realm,
        outbound,
        crypt_state,
        Instant::now(),
    ));

    state.insert_user(Arc::clone(&user));

    // Everything after registration is wrapped so a write/render failure during
    // the handshake gets the same cleanup as a steady-state disconnect.
    let result = async {
        // Lifecycle prelude, then the connection-specific view produced by the
        // P5 engine, then ServerSync/ServerConfig. This is the one live
        // translation path documented by voxloom-session.
        for message in handshake::handshake_prelude(crypt_setup) {
            write_message(&mut writer, &message).await?;
        }
        write_initial_view(&mut writer, &user, &state).await?;
        for message in handshake::handshake_completion(state.config(), session) {
            write_message(&mut writer, &message).await?;
        }
        user.mark_view_live();
        state.refresh_views();

        service_loop(
            &mut reader,
            &mut writer,
            &mut outbound_rx,
            &user,
            &state,
            &udp,
        )
        .await
    }
    .await;

    state.remove_user(session);
    state.refresh_views();

    result
}

/// The steady-state loop: interleave reading client frames with writing frames
/// pushed by other connections (presence) onto this connection's queue.
async fn service_loop(
    reader: &mut FrameReader,
    writer: &mut WriteHalf<TlsStream<TcpStream>>,
    outbound_rx: &mut mpsc::Receiver<ControlMessage>,
    user: &Arc<UserEntry>,
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
                        for reply in handle_control(message, user, state) {
                            write_message(writer, &reply).await?;
                        }
                    }
                }
            }
            // Cancellation-safety: `recv` is cancel-safe; a message is only taken
            // from the channel when this branch is selected.
            queued = outbound_rx.recv() => {
                match queued {
                    Some(message) => {
                        write_message(writer, &message).await?;
                        // A transition refused for ordinary congestion is
                        // retried as capacity returns. Planning always starts
                        // from the still-committed view, so this converges to
                        // the newest desired state rather than replaying stale
                        // intermediate ones.
                        state.refresh_view(user);
                    }
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
fn handle_control(
    message: ControlMessage,
    user: &Arc<UserEntry>,
    state: &SharedState,
) -> Vec<ControlMessage> {
    let session = user.session;
    match message {
        // TCP ping: echo the timestamp so the client can measure RTT (§16.3),
        // plus our own crypt counters.
        ControlMessage::Ping(ping) => vec![ControlMessage::Ping(ping_reply(&ping, user))],

        // Everything else a client may send is, in P3, either a self-state change
        // we do not yet reflect or an action we refuse by default (spec §16.5-16.8,
        // §16.11...). Drop it explicitly rather than acting on it (fail closed).
        other => match user.view.lock() {
            Ok(view) => match view.resolve_inbound(&other) {
                Ok(InboundCommand::MoveSelf { channel }) => {
                    drop(view);
                    match projection::realm_from_channel_key(&channel) {
                        Some(realm) if state.move_to_realm(session, realm) => Vec::new(),
                        _ => vec![permission_denied(session)],
                    }
                }
                Ok(InboundCommand::QueryPermissions {
                    channel,
                    permissions,
                }) => {
                    let channel_id = view
                        .committed()
                        .channels
                        .values()
                        .find(|candidate| candidate.key == channel)
                        .map(|candidate| candidate.id.0);
                    vec![ControlMessage::PermissionQuery(tcp::PermissionQuery {
                        channel_id,
                        permissions: Some(wire_permissions(permissions)),
                        ..Default::default()
                    })]
                }
                Ok(InboundCommand::ValidatedUnsupported { .. }) | Err(_) => {
                    drop_unsupported(&other, session);
                    vec![permission_denied(session)]
                }
            },
            Err(_) => {
                user.outbound.mark_fatal();
                Vec::new()
            }
        },
    }
}

fn permission_denied(session: SessionId) -> ControlMessage {
    ControlMessage::PermissionDenied(tcp::PermissionDenied {
        session: Some(session),
        reason: Some("This action is not available in the current view".to_owned()),
        r#type: Some(i32::from(tcp::permission_denied::DenyType::Text)),
        ..Default::default()
    })
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

async fn write_initial_view(
    writer: &mut WriteHalf<TlsStream<TcpStream>>,
    user: &Arc<UserEntry>,
    state: &SharedState,
) -> Result<()> {
    let scenario = state.scenario_users();
    let viewer = scenario
        .iter()
        .find(|candidate| candidate.session == user.session)
        .context("new session missing from scenario snapshot")?;
    let (steps, token) = {
        let mut view = user
            .view
            .lock()
            .map_err(|_| anyhow::anyhow!("session {} view lock poisoned", user.session))?;
        let (desired, routes) = projection::render(&mut view, viewer, &scenario, state.config())
            .context("rendering view")?;
        view.prepare(&desired, &routes)
            .context("preparing initial view")?
            .context("initial view unexpectedly produced no transition")?
            .split()
    };
    let mut enables = Vec::new();
    for step in steps {
        match step {
            EmittedStep::Message(message) => write_message(writer, &message).await?,
            EmittedStep::RouteChange {
                route,
                enabled: false,
            } => state.set_route(route, false),
            EmittedStep::RouteChange {
                route,
                enabled: true,
            } => enables.push(route),
        }
    }
    user.view
        .lock()
        .map_err(|_| anyhow::anyhow!("session {} view lock poisoned", user.session))?
        .commit(token)
        .context("committing initial view")?;
    for route in enables {
        state.set_route(route, true);
    }
    Ok(())
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
