//! One task per TCP connection: terminate TLS, route, attach, serve, detach.
//!
//! The task owns the socket and nothing else. A migration never moves it: only
//! the shard it sends its events to changes, which is exactly why a connection
//! can cross shards without its client noticing anything but a new tree.
//!
//! ```text
//!   TLS -> Version -> [Authenticate] -> router.route()
//!        -> attach -> prelude -> the shard's first transition -> ServerSync
//!        -> service loop
//!        -> detach
//! ```

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
use voxloom_shard::{ChannelId, OutboundQueue, ShardCommand, TextTarget};

use crate::config::GatewayConfig;
use crate::handshake;
use crate::limits;
use crate::peer::{ClientReport, Peer, ShardPlane};
use crate::router::{ConnectionIdentity, ConnectionRouter, RouteDecision};
use crate::runtime::RuntimeHandle;
use crate::voice::{VoicePlane, generate_crypt_setup};

/// How long the handshake waits for the shard to publish this connection's
/// first view.
///
/// Generous, because the wait is only ever one shard turn plus scheduling. It
/// exists so a shard wedged by a flavor's own bug refuses the arrival instead of
/// leaving a client staring at a handshake that never completes.
const FIRST_VIEW_TIMEOUT: Duration = Duration::from_secs(5);

/// Serve one accepted TCP connection to completion.
///
/// # Errors
///
/// A TLS failure, a malformed frame or a dropped socket. Any of them ends the
/// connection cleanly: it is detached and its peer record removed.
pub async fn serve<R: ConnectionRouter>(
    tcp: TcpStream,
    acceptor: TlsAcceptor,
    runtime: RuntimeHandle,
    router: Arc<R>,
    voice: Arc<VoicePlane>,
    udp: Arc<UdpSocket>,
    config: Arc<GatewayConfig>,
) -> Result<()> {
    // A fresh randomness handle per connection; `SystemRandom` is a cheap ZST.
    let rng = SystemRandom::new();
    // Nagle off: control latency matters more than coalescing small frames.
    let _ignored = tcp.set_nodelay(true);
    let host = tcp.peer_addr().context("TCP peer address")?.ip();

    let tls = acceptor.accept(tcp).await.context("TLS handshake")?;
    let certificate_hash = crate::tls::client_certificate_hash(&tls);
    let (read_half, write_half) = tokio::io::split(tls);
    let mut reader = FrameReader::new(read_half);
    let mut writer = write_half;

    // The server announces its version as soon as TLS completes, before the
    // client authenticates (REF Server.cpp::encrypted).
    write_message(&mut writer, &handshake::server_version(&config)).await?;

    let Some(authenticate) = wait_for_authenticate(&mut reader).await? else {
        return Ok(()); // it left before authenticating
    };

    let identity = ConnectionIdentity {
        // The client's proposal, bounded before it is ever stored or rendered.
        name: authenticate
            .username
            .as_deref()
            .unwrap_or("Guest")
            .chars()
            .take(64)
            .collect(),
        certificate_hash,
        credential: authenticate.password,
    };

    if runtime.peers().len() >= config.max_users as usize {
        write_message(&mut writer, &handshake::reject("the server is full")).await?;
        return Ok(());
    }

    // Both identifiers are reserved before the routing decision, so the router
    // can bind this connection's claimed identity to the id every later event
    // will carry. Neither is ever reused, so reserving one for a connection that
    // is then rejected costs a number and nothing else.
    let connection = runtime.next_connection();
    let session = runtime
        .session_for(connection)
        .context("allocating a session")?;

    let shard = match router.route(connection, &identity).await {
        RouteDecision::Attach(shard) => shard,
        RouteDecision::Reject(reason) => {
            write_message(&mut writer, &handshake::reject(&reason)).await?;
            return Ok(());
        }
    };

    let (crypt_setup, crypt_state) = generate_crypt_setup(&rng)?;
    let (queue, mut outbound) = OutboundQueue::new();

    // The plane is a placeholder until `attach` points it at the real shard.
    // Registering first is what lets the shard's very first render already find
    // the connection in the peer table.
    let placeholder = ShardPlane {
        shard,
        routing: tokio::sync::watch::channel(Arc::new(voxloom_shard::AudioRouting::default())).1,
    };
    let peer = Arc::new(Peer::new(
        connection,
        session,
        host,
        crypt_state,
        Arc::new(queue),
        placeholder,
        Instant::now(),
    ));
    runtime.peers().insert(Arc::clone(&peer));

    // Everything past registration is wrapped so a failure during the handshake
    // gets exactly the same cleanup as a steady-state disconnect.
    let result = async {
        for message in handshake::prelude(crypt_setup) {
            write_message(&mut writer, &message).await?;
        }

        let ready = runtime
            .attach(&peer, shard)
            .context("attaching to the routed shard")?;

        // The first transition is the ordinary one: the shard plans it, the
        // queue carries it, and the handshake merely writes it out before
        // `ServerSync`. That is what keeps invariants 1 and 6 satisfied with a
        // single rendering path in the runtime.
        tokio::time::timeout(FIRST_VIEW_TIMEOUT, ready)
            .await
            .context("the shard did not publish a first view in time")?
            .context("the shard dropped the connection during its first render")?;
        drain(&mut writer, &mut outbound).await?;

        for message in handshake::completion(&config, session) {
            write_message(&mut writer, &message).await?;
        }

        service(
            &mut reader,
            &mut writer,
            &mut outbound,
            &Serving {
                peer: &peer,
                runtime: &runtime,
                voice: &voice,
                udp: &udp,
                config: &config,
            },
        )
        .await
    }
    .await;

    runtime.peers().remove(connection);
    runtime.detach(connection, peer.shard(), "connection closed");

    result
}

/// Everything the service loop needs from the gateway around one connection.
///
/// Grouped rather than passed one by one: they share a lifetime, none of them
/// changes while a connection lives, and a handler that needs three of them
/// should not have to say so in its signature.
struct Serving<'a> {
    peer: &'a Arc<Peer>,
    runtime: &'a RuntimeHandle,
    voice: &'a Arc<VoicePlane>,
    udp: &'a UdpSocket,
    config: &'a GatewayConfig,
}

/// The steady state: read client frames, write what the shard pushed.
async fn service(
    reader: &mut FrameReader,
    writer: &mut WriteHalf<TlsStream<TcpStream>>,
    outbound: &mut mpsc::Receiver<ControlMessage>,
    serving: &Serving<'_>,
) -> Result<()> {
    let Serving {
        peer,
        runtime,
        voice,
        udp,
        config,
    } = *serving;
    loop {
        // A refused control message means this client is too far behind to hold
        // a correct view, so the connection ends and reconnecting rebuilds one.
        //
        // Polling rather than being woken is sound: the flag is only set when
        // the queue is full, which means there are messages waiting, so the
        // branch below fires immediately and comes straight back here.
        if peer.queue().must_close() {
            anyhow::bail!(
                "session {:?}: output queue overflowed, closing rather than diverging",
                peer.session()
            );
        }

        tokio::select! {
            // Cancellation-safe: `next` reads into an owned buffer and never
            // leaves a half-consumed frame across an await, so dropping it on
            // the other branch loses nothing.
            incoming = reader.next() => {
                match incoming? {
                    None => return Ok(()), // clean close
                    Some(ControlMessage::UdpTunnel(raw)) => {
                        for (sealed, to) in tunnelled(voice, peer, &raw) {
                            if let Err(error) = udp.send_to(&sealed, to).await {
                                eprintln!("voxloom-gateway: UDP send to {to} failed: {error}");
                            }
                        }
                    }
                    Some(message) => {
                        for reply in inbound(message, peer, runtime, config) {
                            write_message(writer, &reply).await?;
                        }
                    }
                }
            }
            // Cancellation-safe: a message leaves the channel only when this
            // branch is selected.
            queued = outbound.recv() => {
                match queued {
                    Some(message) => {
                        let was_voice = matches!(message, ControlMessage::UdpTunnel(_));
                        write_message(writer, &message).await?;
                        // Draining control capacity is what a congested shard is
                        // waiting for, so tell it - for this connection alone,
                        // in O(1). Draining a tunnelled voice packet frees
                        // nothing worth re-rendering for, and it happens fifty
                        // times a second per speaker.
                        if !was_voice {
                            let _delivered = runtime.send(
                                peer.shard(),
                                ShardCommand::Drained(peer.connection()),
                            );
                        }
                    }
                    // The queue's sending half lives in the shard; losing it
                    // means the shard is gone.
                    None => return Ok(()),
                }
            }
        }
    }
}

/// Read frames until `Authenticate` arrives, or the client leaves.
async fn wait_for_authenticate(reader: &mut FrameReader) -> Result<Option<tcp::Authenticate>> {
    loop {
        match reader.next().await? {
            None => return Ok(None),
            Some(ControlMessage::Authenticate(authenticate)) => return Ok(Some(authenticate)),
            // The client's own Version, and any other pre-auth chatter, is
            // accepted and ignored.
            Some(_) => continue,
        }
    }
}

/// Handle one control message from the client.
///
/// Every branch ends in an explicit outcome (R6): a reply, a request forwarded
/// to the shard, or a logged refusal.
fn inbound(
    message: ControlMessage,
    peer: &Arc<Peer>,
    runtime: &RuntimeHandle,
    config: &GatewayConfig,
) -> Vec<ControlMessage> {
    if unidles(&message) {
        peer.record_activity(Instant::now());
    }

    match message {
        // REF: references/mumble/src/murmur/Messages.cpp : `Server::msgPing`
        //   stores what the client reports about its own side, then answers with
        //   the timestamp and the server's OCB2 counters.
        ControlMessage::Ping(ping) => {
            peer.record_report(reported(&ping));
            vec![ControlMessage::Ping(ping_reply(&ping, peer))]
        }

        ControlMessage::UserState(state) => user_state(&state, peer, runtime),

        // Both are questions rather than intents, so the flavor never sees them:
        // the shard answers from the render it already published, for the asking
        // connection alone.
        //
        // REF: references/mumble/src/mumble/MainWindow.cpp : the client asks
        //   these of its own accord, on channel selection and on opening a
        //   user's information window.
        ControlMessage::PermissionQuery(query) => match query.channel_id {
            Some(channel) => {
                let _delivered = runtime.send(
                    peer.shard(),
                    ShardCommand::QueriedPermissions {
                        connection: peer.connection(),
                        channel: ChannelId(channel),
                    },
                );
                Vec::new()
            }
            // `flush` is the server's word to the client, and a query about no
            // channel at all has no answer.
            None => {
                refused("PermissionQuery naming no channel", peer);
                vec![permission_denied(peer)]
            }
        },

        ControlMessage::UserStats(request) => user_stats(&request, peer, runtime),

        ControlMessage::TextMessage(text) => text_message(&text, peer, runtime, config),

        // An intent, so it goes to the shard, which alone knows what this
        // connection was offered and what it can see. Nothing is validated here:
        // the gateway holds no view, and guessing would only mean refusing a
        // legitimate button.
        //
        // REF: references/mumble/src/murmur/Messages.cpp : `msgContextAction`
        //   uses `MSG_SETUP`, so it counts as activity like any other intent.
        ControlMessage::ContextAction(action) => {
            let _delivered = runtime.send(
                peer.shard(),
                ShardCommand::InvokedAction {
                    connection: peer.connection(),
                    action: action.action,
                    session: action.session.map(voxloom_shard::SessionId),
                    channel: action.channel_id.map(ChannelId),
                },
            );
            Vec::new()
        }

        other => {
            refused(kind_of(&other), peer);
            vec![permission_denied(peer)]
        }
    }
}

/// Handle a `UserState` a client sent about itself.
///
/// Both intents this build understands are forwarded as *requests*: the flavor
/// decides, and until it renders something new nothing about the view changes.
/// Anything aimed at another session is moderation, which no flavor here
/// exposes, so it is refused.
///
/// A `UserState` carrying **no** session at all is about its own sender. That is
/// not a leniency, it is how the official client mutes itself.
///
/// REF: references/mumble/src/murmur/Messages.cpp : `VICTIM_SETUP` starts from
///   `uSource` and only looks a session up when the message carries one.
/// REF: references/mumble/src/mumble/ServerHandler.cpp : `setSelfMuteDeafState`
///   sends a `UserState` with both flags and no session.
fn user_state(
    state: &tcp::UserState,
    peer: &Arc<Peer>,
    runtime: &RuntimeHandle,
) -> Vec<ControlMessage> {
    let mine = state
        .session
        .is_none_or(|session| session == peer.session().0);
    if !mine {
        refused("UserState aimed at another session", peer);
        return vec![permission_denied(peer)];
    }

    let mut forwarded = false;

    // "Put me in that channel", the double-click. A channel the connection
    // cannot see is refused by the shard, which is what keeps a guessed id from
    // working as an existence oracle.
    //
    // REF: references/vendored/Mumble.proto : a client moves itself with a
    //   `UserState` naming its own session and a `channel_id`.
    if let Some(channel) = state.channel_id {
        let _delivered = runtime.send(
            peer.shard(),
            ShardCommand::Requested {
                connection: peer.connection(),
                channel: ChannelId(channel),
            },
        );
        forwarded = true;
    }

    let (self_mute, self_deaf) = self_state(state);
    if self_mute.is_some() || self_deaf.is_some() {
        let _delivered = runtime.send(
            peer.shard(),
            ShardCommand::RequestedSelfState {
                connection: peer.connection(),
                self_mute,
                self_deaf,
            },
        );
        forwarded = true;
    }

    if forwarded {
        return Vec::new();
    }

    // Recording announcements, plugin context, listener registrations and
    // temporary access tokens all land here. None of them is mirrored into a
    // view, and acknowledging them silently would be a lie (R6).
    refused("UserState", peer);
    vec![permission_denied(peer)]
}

/// Handle a `TextMessage` a client typed.
///
/// Everything decided here is a property of the message itself - how fast they
/// arrive, how long it is, what shape its targets have - and nothing is a
/// property of the view, which this side does not hold. Whether the connection
/// may name that target at all is the shard's question, and it is asked there.
///
/// The four refusals are not interchangeable:
///
/// - A flood is dropped with **no answer**, like the reference server's, because
///   answering a flood is participating in it.
/// - An empty message is dropped silently: there is nothing to deliver.
/// - Too long gets `TextTooLong`, which the client has a message for.
/// - Anything else gets the generic refusal.
///
/// REF: references/mumble/src/murmur/Messages.cpp : `msgTextMessage` runs
///   `RATELIMIT`, then `isTextAllowed` with `PERM_DENIED_TYPE(TextTooLong)`,
///   then returns on an empty message, before looking at a single target.
fn text_message(
    text: &tcp::TextMessage,
    peer: &Arc<Peer>,
    runtime: &RuntimeHandle,
    config: &GatewayConfig,
) -> Vec<ControlMessage> {
    if !peer.allow_text(Instant::now()) {
        refused("TextMessage over the rate limit", peer);
        return Vec::new();
    }

    if text.message.trim().is_empty() {
        return Vec::new();
    }

    // Counted in characters where the reference server counts UTF-16 code
    // units. The two agree on everything below the astral planes, and erring
    // towards accepting one emoji-heavy message the reference would have cut is
    // the safer side of a limit that exists to bound a text box.
    let length = u32::try_from(text.message.chars().count()).unwrap_or(u32::MAX);
    if config.message_length > 0 && length > config.message_length {
        refused("TextMessage over the advertised length", peer);
        return vec![ControlMessage::PermissionDenied(tcp::PermissionDenied {
            r#type: Some(i32::from(tcp::permission_denied::DenyType::TextTooLong)),
            ..Default::default()
        })];
    }

    // The reference server strips HTML when it does not allow it. Stripping it
    // correctly is a parser, and a parser fed by clients is the last thing this
    // crate should grow, so a server that turned HTML off refuses markup instead
    // of quietly rewriting it (R6).
    //
    // REF: references/mumble/src/murmur/Server.cpp : `isTextAllowed` runs
    //   `HTMLFilter::filter` when `bAllowHTML` is false.
    if !config.allow_html && text.message.contains('<') {
        refused(
            "TextMessage carrying markup on a server that forbids it",
            peer,
        );
        return vec![ControlMessage::PermissionDenied(tcp::PermissionDenied {
            session: Some(peer.session().0),
            reason: Some("This server does not accept formatted text".to_owned()),
            r#type: Some(i32::from(tcp::permission_denied::DenyType::Text)),
            ..Default::default()
        })];
    }

    let Some(to) = single_target(text) else {
        refused("TextMessage naming no single target", peer);
        return vec![permission_denied(peer)];
    };

    let _delivered = runtime.send(
        peer.shard(),
        ShardCommand::Said {
            connection: peer.connection(),
            to,
            message: text.message.clone(),
        },
    );
    Vec::new()
}

/// The one target a `TextMessage` names, or nothing.
///
/// The official client fills exactly one of the three lists with exactly one
/// identifier, so anything else is either a different client with a fan-out this
/// server has not agreed to, or a probe. Both are refused.
///
/// REF: references/mumble/src/mumble/ServerHandler.cpp :
///   `sendUserTextMessage` adds one session; `sendChannelTextMessage` adds one
///   `channel_id`, or one `tree_id` for the tree variant.
fn single_target(text: &tcp::TextMessage) -> Option<TextTarget> {
    match (
        text.session.as_slice(),
        text.channel_id.as_slice(),
        text.tree_id.as_slice(),
    ) {
        ([session], [], []) => Some(TextTarget::Session(voxloom_shard::SessionId(*session))),
        ([], [channel], []) => Some(TextTarget::Channel(ChannelId(*channel))),
        ([], [], [tree]) => Some(TextTarget::Tree(ChannelId(*tree))),
        _ => None,
    }
}

/// Handle a client's `UserStats` question.
///
/// Split in two because the two halves know different things. What the runtime
/// publishes about *another* user is a view question, so the shard answers it
/// and only for a user this connection can see. What it knows about the asker
/// itself is a transport question - the OCB2 counters live here, in the peer -
/// and it is the same triplet every `Ping` reply already carries, so answering
/// discloses nothing new.
///
/// A `UserStats` with no session at all is about its sender, like every other
/// message that omits it.
///
/// REF: references/mumble/src/murmur/Messages.cpp : `msgUserStats` answers the
///   full detail only for `extend` - self, or Ban at the root - and the packet
///   counters only for `local`.
fn user_stats(
    request: &tcp::UserStats,
    peer: &Arc<Peer>,
    runtime: &RuntimeHandle,
) -> Vec<ControlMessage> {
    let target = request.session.unwrap_or(peer.session().0);
    if target == peer.session().0 {
        return vec![own_stats(peer, Instant::now())];
    }

    let _delivered = runtime.send(
        peer.shard(),
        ShardCommand::QueriedUserStats {
            connection: peer.connection(),
            target: voxloom_shard::SessionId(target),
        },
    );
    Vec::new()
}

/// Whether a message means somebody is still there.
///
/// A keepalive and the two questions a client asks on its own do not: a window
/// left open polling for statistics would otherwise keep an idle user looking
/// active forever.
///
/// REF: references/mumble/src/murmur/Messages.cpp : `MSG_SETUP` calls
///   `resetIdleSeconds()` while `MSG_SETUP_NO_UNIDLE` does not, and the second
///   is used by `msgPing`, `msgCryptSetup`, `msgVoiceTarget`,
///   `msgPermissionQuery`, `msgCodecVersion`, `msgUserStats` and
///   `msgRequestBlob`.
fn unidles(message: &ControlMessage) -> bool {
    !matches!(
        message,
        ControlMessage::Ping(_)
            | ControlMessage::CryptSetup(_)
            | ControlMessage::VoiceTarget(_)
            | ControlMessage::PermissionQuery(_)
            | ControlMessage::CodecVersion(_)
            | ControlMessage::UserStats(_)
            | ControlMessage::RequestBlob(_)
    )
}

/// What a client reports about its own side of the link, in every `Ping`.
///
/// Kept verbatim, exactly as the reference server keeps it, because none of it
/// is measurable from here: the loss the client sees, its own ping to us, the
/// packets it counted. It only ever travels back to the client that sent it.
///
/// REF: references/mumble/src/murmur/Messages.cpp : `msgPing` assigns each of
///   these straight from the message, then answers with the server's own
///   counters.
fn reported(ping: &tcp::Ping) -> ClientReport {
    ClientReport {
        good: ping.good.unwrap_or_default(),
        late: ping.late.unwrap_or_default(),
        lost: ping.lost.unwrap_or_default(),
        resync: ping.resync.unwrap_or_default(),
        udp_packets: ping.udp_packets.unwrap_or_default(),
        tcp_packets: ping.tcp_packets.unwrap_or_default(),
        udp_ping_avg: ping.udp_ping_avg.unwrap_or_default(),
        udp_ping_var: ping.udp_ping_var.unwrap_or_default(),
        tcp_ping_avg: ping.tcp_ping_avg.unwrap_or_default(),
        tcp_ping_var: ping.tcp_ping_var.unwrap_or_default(),
    }
}

/// What this connection may be told about itself.
///
/// Three kinds of number, and they are not worth the same:
///
/// - `from_client` is the **server's** decryption tally for this peer, the one
///   thing here the client cannot know. It is the same triplet its `Ping`
///   replies already carry.
/// - `bandwidth` and the two times are measured here too.
/// - everything else is the client's own report, handed straight back. It tells
///   the client nothing new, but the information window hides the whole UDP
///   block unless **both** halves are present, so the mirror is what makes the
///   half that matters visible at all.
///
/// The certificate chain, the client version and the IP address are left out on
/// purpose: a connection already knows all three about itself, and holding a DER
/// chain per peer to fill a dialog is memory spent on nothing.
///
/// REF: references/mumble/src/mumble/UserInformation.cpp : the dialog calls
///   `qgbUDP->setVisible(false)` unless `has_from_client() && has_from_server()`,
///   and prints `bandwidth / 125.0` as kbit/s.
fn own_stats(peer: &Peer, now: Instant) -> ControlMessage {
    let (good, late, lost) = peer.crypt_counters();
    let reported = peer.reported();
    let (bandwidth, idle) = peer.traffic(now);
    let seconds =
        |duration: std::time::Duration| u32::try_from(duration.as_secs()).unwrap_or(u32::MAX);

    ControlMessage::UserStats(tcp::UserStats {
        session: Some(peer.session().0),
        from_client: Some(tcp::user_stats::Stats {
            good: Some(good),
            late: Some(late),
            lost: Some(lost),
            // Nonce resync is refused, so zero is the count rather than a
            // placeholder, exactly as in the `Ping` reply.
            resync: Some(0),
        }),
        from_server: Some(tcp::user_stats::Stats {
            good: Some(reported.good),
            late: Some(reported.late),
            lost: Some(reported.lost),
            resync: Some(reported.resync),
        }),
        udp_packets: Some(reported.udp_packets),
        tcp_packets: Some(reported.tcp_packets),
        udp_ping_avg: Some(reported.udp_ping_avg),
        udp_ping_var: Some(reported.udp_ping_var),
        tcp_ping_avg: Some(reported.tcp_ping_avg),
        tcp_ping_var: Some(reported.tcp_ping_var),
        bandwidth: Some(bandwidth),
        onlinesecs: Some(seconds(now.saturating_duration_since(peer.online_since()))),
        idlesecs: Some(seconds(idle)),
        ..Default::default()
    })
}

/// The self-mute and self-deafen a `UserState` asks for, with the two
/// implications that need no memory of the current state.
///
/// Deafened implies muted, and unmuting undeafens. Both are what the reference
/// server does, and applying them at the protocol boundary keeps the flavor from
/// having to know a Mumble rule. The order matters: a message asking to be
/// deafened while unmuted resolves to "both", exactly as Murmur resolves it,
/// because the deafen rewrite runs first and the unmute test then sees the
/// rewritten value.
///
/// A flag the client did not mention stays `None`, because only the flavor knows
/// what it currently renders.
///
/// REF: references/mumble/src/murmur/Messages.cpp : `msgUserState` sets
///   `self_mute` when `self_deaf` is true, then clears `self_deaf` when
///   `self_mute` is false.
fn self_state(state: &tcp::UserState) -> (Option<bool>, Option<bool>) {
    let mut self_mute = state.self_mute;
    let mut self_deaf = state.self_deaf;

    if self_deaf == Some(true) {
        self_mute = Some(true);
    }
    if self_mute == Some(false) {
        self_deaf = Some(false);
    }

    (self_mute, self_deaf)
}

/// Route a TCP-tunnelled voice packet (the UDP fallback of spec 15.6).
///
/// The payload is a plaintext UDP packet, so once decoded it goes through the
/// very same routing as a datagram. Recipients on UDP are returned as datagrams;
/// recipients on the tunnel are served inside the voice plane.
fn tunnelled(voice: &Arc<VoicePlane>, peer: &Arc<Peer>, raw: &[u8]) -> Vec<(Vec<u8>, SocketAddr)> {
    // The client is telling us its UDP does not work, so its own audio goes back
    // over the tunnel until a datagram from it reaches us again.
    // REF: references/mumble/src/murmur/Server.cpp : the `UDPTunnel` branch sets
    //   `u->aiUdpFlag = 0`.
    peer.set_udp_mode(false);

    if !limits::is_acceptable_size(raw.len()) {
        eprintln!(
            "voxloom-gateway: session {:?}: dropping a {}-byte tunnelled packet",
            peer.session(),
            raw.len()
        );
        return Vec::new();
    }

    match decode_udp(raw) {
        Ok(UdpMessage::Audio(audio)) => voice.route(peer, &audio, Instant::now(), raw.len()),
        Ok(UdpMessage::Ping(_)) => {
            // Connectivity pings belong on the UDP socket; one arriving here
            // measures nothing, so it is refused rather than answered.
            eprintln!(
                "voxloom-gateway: session {:?}: refusing a ping through the tunnel",
                peer.session()
            );
            Vec::new()
        }
        Err(error) => {
            eprintln!(
                "voxloom-gateway: session {:?}: bad tunnelled envelope: {error}",
                peer.session()
            );
            Vec::new()
        }
    }
}

/// Build the reply to a TCP `Ping`.
///
/// Reporting `good` is not bookkeeping. The client reads it as `uiRemoteGood`
/// and, if it is still zero twenty seconds into the session, decides its UDP
/// never reaches us and falls back to the TCP tunnel permanently - while the UDP
/// plane is working in both directions.
///
/// REF: references/mumble/src/mumble/ServerHandler.cpp : `TCPMessageType::Ping`
///   disables UDP on `(uiRemoteGood == 0 || uiGood == 0) && bUdp && elapsed >
///   20000000`.
fn ping_reply(request: &tcp::Ping, peer: &Peer) -> tcp::Ping {
    let (good, late, lost) = peer.crypt_counters();
    tcp::Ping {
        timestamp: request.timestamp,
        good: Some(good),
        late: Some(late),
        lost: Some(lost),
        // Nonce resync is refused, so zero is the true count rather than a
        // placeholder.
        resync: Some(0),
        ..Default::default()
    }
}

/// REF: references/vendored/Mumble.proto : `PermissionDenied`.
fn permission_denied(peer: &Peer) -> ControlMessage {
    ControlMessage::PermissionDenied(tcp::PermissionDenied {
        session: Some(peer.session().0),
        reason: Some("This action is not available in the current view".to_owned()),
        r#type: Some(i32::from(tcp::permission_denied::DenyType::Text)),
        ..Default::default()
    })
}

/// Name the drop so it is auditable rather than silent (R6).
fn refused(kind: &str, peer: &Peer) {
    eprintln!(
        "voxloom-gateway: session {:?}: refusing {kind}",
        peer.session()
    );
}

fn kind_of(message: &ControlMessage) -> &'static str {
    match message {
        ControlMessage::ChannelState(_) => "ChannelState",
        ControlMessage::ChannelRemove(_) => "ChannelRemove",
        ControlMessage::UserRemove(_) => "UserRemove",
        ControlMessage::Acl(_) => "ACL",
        ControlMessage::VoiceTarget(_) => "VoiceTarget",
        ControlMessage::CryptSetup(_) => "CryptSetup(resync)",
        ControlMessage::RequestBlob(_) => "RequestBlob",
        _ => "an unsupported message",
    }
}

/// Write everything already queued, then return.
///
/// `try_recv` is deliberate: the queue holds exactly what the shard's first
/// transition put there, and waiting for more would wait forever.
async fn drain(
    writer: &mut WriteHalf<TlsStream<TcpStream>>,
    outbound: &mut mpsc::Receiver<ControlMessage>,
) -> Result<()> {
    while let Ok(message) = outbound.try_recv() {
        write_message(writer, &message).await?;
    }
    Ok(())
}

async fn write_message(
    writer: &mut WriteHalf<TlsStream<TcpStream>>,
    message: &ControlMessage,
) -> Result<()> {
    let mut framed = Vec::new();
    encode_frame(message, &mut framed).context("encoding a control frame")?;
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
    fn new(read: ReadHalf<TlsStream<TcpStream>>) -> FrameReader {
        FrameReader {
            read,
            buffer: Vec::with_capacity(4096),
        }
    }

    /// The next complete control message, or `None` at a clean EOF on a frame
    /// boundary. Reassembles across TLS records.
    async fn next(&mut self) -> Result<Option<ControlMessage>> {
        loop {
            if let Some((message, consumed)) = self.try_parse()? {
                self.buffer.drain(..consumed);
                return Ok(Some(message));
            }

            let mut chunk = [0u8; 4096];
            let read = self.read.read(&mut chunk).await.context("TCP read")?;
            if read == 0 {
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                anyhow::bail!(
                    "connection closed mid-frame ({} bytes buffered)",
                    self.buffer.len()
                );
            }
            self.buffer
                .extend_from_slice(chunk.get(..read).unwrap_or_default());
        }
    }

    fn try_parse(&self) -> Result<Option<(ControlMessage, usize)>> {
        match parse_frame(&self.buffer).context("framing")? {
            Some(frame) => {
                let consumed = frame.total_len();
                let message = decode_frame(&frame).context("decoding a control message")?;
                Ok(Some((message, consumed)))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked(self_mute: Option<bool>, self_deaf: Option<bool>) -> (Option<bool>, Option<bool>) {
        self_state(&tcp::UserState {
            self_mute,
            self_deaf,
            ..Default::default()
        })
    }

    /// The whole table, because the two implications interact and the
    /// interesting cases are the contradictory ones.
    ///
    /// REF: references/mumble/src/murmur/Messages.cpp : `msgUserState`. Each row
    /// is what that code leaves in the broadcast message for the same input.
    #[test]
    fn the_self_state_resolves_the_way_the_reference_server_resolves_it() {
        // What the official client always sends: both flags, no contradiction.
        assert_eq!(asked(Some(true), Some(false)), (Some(true), Some(false)));
        assert_eq!(asked(Some(false), Some(false)), (Some(false), Some(false)));
        assert_eq!(asked(Some(true), Some(true)), (Some(true), Some(true)));

        // Deafened wins over unmuted: Murmur overwrites `self_mute` before it
        // ever reads it, so "deafen me but leave me unmuted" means both.
        assert_eq!(asked(Some(false), Some(true)), (Some(true), Some(true)));

        // One flag alone. Deafening still implies muting; unmuting still
        // undeafens; the two that imply nothing leave the other untouched.
        assert_eq!(asked(None, Some(true)), (Some(true), Some(true)));
        assert_eq!(asked(Some(false), None), (Some(false), Some(false)));
        assert_eq!(asked(Some(true), None), (Some(true), None));
        assert_eq!(asked(None, Some(false)), (None, Some(false)));
    }

    #[test]
    fn a_user_state_about_nothing_asks_for_nothing() {
        // The guard that keeps `user_state` from forwarding an empty request,
        // and therefore from acknowledging one.
        assert_eq!(asked(None, None), (None, None));
    }
}
