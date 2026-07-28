//! The UDP voice plane. **No shard task takes part in it.**
//!
//! ```text
//!   1. recv_from(addr)
//!   2. peers.by_address(addr)        -> one hash; absent means the cold path
//!   3. peer.decrypt(datagram)        -> this connection's OCB2 domain alone
//!   4. peer.routing()                -> the shard's table, an Arc read
//!   5. routing.receivers(sender)     -> a borrowed slice, no allocation
//!   6. per receiver, if the cursor gate passes: encrypt with ITS key,
//!      then send_to, or push onto its queue for the TCP tunnel
//! ```
//!
//! The gate at step 6 is the whole of the audio ordering protocol:
//!
//! ```text
//!   receiver.cursor >= routing.since(sender)
//! ```
//!
//! One atomic load and one comparison. It is conservative on purpose - a lagging
//! receiver loses audio from speakers it already knew about, at worst a few
//! hundred milliseconds of silence for a client already in trouble - and it
//! replaces every handshake a "is the view ready" protocol would have needed.
//!
//! Revocation needs no gate at all: the shard publishes its table before it
//! pushes any view, and a route that is gone is simply absent. Cutting too early
//! is always safe; you hear less than your due, never more.
//!
//! REF: docs/design/guide-implementation.md 9.5, 9.7

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use ring::rand::{SecureRandom, SystemRandom};
use tokio::net::UdpSocket;
use mumble_server_runtime_crypto::{BLOCK_SIZE, CryptState, KEY_SIZE};
use mumble_server_runtime_protocol::messages::{tcp, udp};
use mumble_server_runtime_protocol::{ControlMessage, UdpMessage, decode_udp, encode_udp};
use mumble_server_runtime_shard::VoiceAdmission;

use crate::config::GatewayConfig;
use crate::limits;
use crate::peer::{Peer, Peers};

/// A sealed datagram and where it goes.
pub type Datagram = (Vec<u8>, SocketAddr);

/// The wire value of the server loopback target.
///
/// REF: references/vendored/MumbleUDP.proto : `Audio.target` - "2^5-1 = 31" is
///   documented as "server loopback".
const LOOPBACK_TARGET: u32 = 31;

/// Normal speech, client to server.
const NORMAL_TARGET: u32 = 0;

/// Normal speech, server to client.
///
/// REF: references/vendored/MumbleUDP.proto : `Audio.context` - "0: Normal
///   speech, 1: Shout to channel, 2: Whisper to user".
const NORMAL_CONTEXT: u32 = 0;

/// Generate a fresh OCB2 key and the two nonces for a new connection.
///
/// REF: references/mumble/src/murmur/Messages.cpp : `msgAuthenticate` sets
///   `server_nonce = getEncryptIV()` and `client_nonce = getDecryptIV()`, so on
///   the server the encrypt IV is the server nonce (S2C) and the decrypt IV is
///   the client nonce (C2S).
///
/// # Errors
///
/// When the system randomness source fails.
pub fn generate_crypt_setup(rng: &SystemRandom) -> Result<(tcp::CryptSetup, CryptState)> {
    let mut key = [0u8; KEY_SIZE];
    let mut server_nonce = [0u8; BLOCK_SIZE];
    let mut client_nonce = [0u8; BLOCK_SIZE];
    rng.fill(&mut key)
        .map_err(|_unspecified| anyhow::anyhow!("rng failed for the OCB2 key"))?;
    rng.fill(&mut server_nonce)
        .map_err(|_unspecified| anyhow::anyhow!("rng failed for the server nonce"))?;
    rng.fill(&mut client_nonce)
        .map_err(|_unspecified| anyhow::anyhow!("rng failed for the client nonce"))?;

    let state = CryptState::new(&key, &server_nonce, &client_nonce);
    let setup = tcp::CryptSetup {
        key: Some(key.to_vec()),
        server_nonce: Some(server_nonce.to_vec()),
        client_nonce: Some(client_nonce.to_vec()),
    };
    Ok((setup, state))
}

/// The UDP plane.
pub struct VoicePlane {
    socket: Arc<UdpSocket>,
    peers: Arc<Peers>,
    config: GatewayConfig,
}

impl VoicePlane {
    #[must_use]
    pub fn new(socket: Arc<UdpSocket>, peers: Arc<Peers>, config: GatewayConfig) -> VoicePlane {
        VoicePlane {
            socket,
            peers,
            config,
        }
    }

    /// Service datagrams until the socket errors. Runs for the life of the
    /// gateway.
    ///
    /// # Errors
    ///
    /// Only when the socket itself fails. One bad datagram never ends the plane.
    pub async fn run(&self) -> Result<()> {
        // Larger reads would be truncated by `recv_from` anyway, and the size
        // band refuses anything near this.
        let mut buffer = vec![0u8; 2048];
        loop {
            let (len, from) = self
                .socket
                .recv_from(&mut buffer)
                .await
                .context("UDP recv_from")?;
            let datagram = buffer.get(..len).unwrap_or_default();

            for (sealed, to) in self.handle_datagram(datagram, from) {
                if let Err(error) = self.socket.send_to(&sealed, to).await {
                    // One failed send is not fatal to the plane.
                    eprintln!("mumble-server-runtime-gateway: UDP send to {to} failed: {error}");
                }
            }
        }
    }

    /// Process one datagram. All crypto is synchronous and happens here; the
    /// caller performs the awaited sends.
    ///
    /// Every path ends in an explicit outcome (R6): datagrams to send, a queued
    /// tunnel message, or a logged drop.
    pub fn handle_datagram(&self, datagram: &[u8], from: SocketAddr) -> Vec<Datagram> {
        // Hot path: an address we have already proven.
        if let Some(peer) = self.peers.by_address(from) {
            return match peer.decrypt(datagram) {
                Some(plaintext) => self.dispatch(&peer, &plaintext, from),
                None => {
                    // Replay, tamper or a desynchronised nonce. Dropping is the
                    // only safe answer; resync handling is future work.
                    eprintln!(
                        "mumble-server-runtime-gateway: undecryptable datagram from bound session {:?} at {from}",
                        peer.session()
                    );
                    Vec::new()
                }
            };
        }

        // Cold path: try the connections whose TCP peer shares this host.
        // Narrowing by host is what keeps this from being O(connections) for
        // every stray packet on the internet.
        for peer in self.peers.candidates(from.ip()) {
            if let Some(plaintext) = peer.decrypt(datagram) {
                self.peers.bind(from, &peer);
                return self.dispatch(&peer, &plaintext, from);
            }
        }

        // Not encrypted for anyone: it may be an unencrypted connectivity ping,
        // which the real server answers before any association exists.
        if let Ok(UdpMessage::Ping(ping)) = decode_udp(datagram) {
            return vec![(self.ping_reply(&ping), from)];
        }

        eprintln!("mumble-server-runtime-gateway: dropping an unroutable datagram from {from}");
        Vec::new()
    }

    /// Decode a decrypted packet and act on it.
    fn dispatch(&self, peer: &Arc<Peer>, plaintext: &[u8], from: SocketAddr) -> Vec<Datagram> {
        // The band applies to the decoded Mumble packet, so both ingress paths
        // enforce one rule (spec 15.7).
        if !limits::is_acceptable_size(plaintext.len()) {
            eprintln!(
                "mumble-server-runtime-gateway: dropping a {}-byte packet from session {:?} (outside the band)",
                plaintext.len(),
                peer.session()
            );
            return Vec::new();
        }

        match decode_udp(plaintext) {
            Ok(UdpMessage::Ping(ping)) => {
                let reply = self.ping_reply(&ping);
                peer.encrypt(&reply)
                    .map(|sealed| vec![(sealed, from)])
                    .unwrap_or_default()
            }
            Ok(UdpMessage::Audio(audio)) => {
                // This peer is reachable over UDP again, so its own audio goes
                // back out that way.
                peer.set_udp_mode(true);
                self.route(peer, &audio, Instant::now(), plaintext.len())
            }
            Err(error) => {
                eprintln!(
                    "mumble-server-runtime-gateway: bad envelope from session {:?}: {error}",
                    peer.session()
                );
                Vec::new()
            }
        }
    }

    /// Route one voice packet from `sender`, whichever transport it arrived on.
    ///
    /// Shared by the UDP plane and the TCP tunnel so both apply the same budget,
    /// the same target vocabulary and the same table.
    ///
    /// `bytes` is the decoded packet as it arrived, billed to this connection's
    /// throughput window whichever transport carried it.
    pub fn route(
        &self,
        sender: &Arc<Peer>,
        audio: &udp::Audio,
        now: Instant,
        bytes: usize,
    ) -> Vec<Datagram> {
        if !sender.allow_voice(now, bytes) {
            eprintln!(
                "mumble-server-runtime-gateway: session {:?}: voice packet dropped, budget exhausted",
                sender.session()
            );
            return Vec::new();
        }

        let Some(udp::audio::Header::Target(target)) = audio.header else {
            // `context` is the server-to-client direction and a header-less
            // packet says nothing. Either way there is no intent to honour.
            eprintln!(
                "mumble-server-runtime-gateway: session {:?}: voice packet with no target",
                sender.session()
            );
            return Vec::new();
        };

        match target {
            LOOPBACK_TARGET => self.reflect(sender, audio),
            NORMAL_TARGET => self.speak(sender, audio),
            registered => {
                // Shout and whisper targets are registered with a `VoiceTarget`
                // control message, which this build refuses. Routing them as
                // normal speech would deliver voice to listeners the client
                // never addressed here.
                eprintln!(
                    "mumble-server-runtime-gateway: session {:?}: refusing unregistered voice target {registered}",
                    sender.session()
                );
                Vec::new()
            }
        }
    }

    /// Normal speech: everyone the shard's table says may hear this sender, and
    /// whose view is far enough along to make sense of it.
    fn speak(&self, sender: &Arc<Peer>, audio: &udp::Audio) -> Vec<Datagram> {
        // Read once for the whole packet, so every receiver is decided against
        // the same table: a change mid-loop cannot deliver half a packet under
        // one policy and half under the next.
        let routing = sender.routing();
        let session = sender.session();
        let since = routing.since(session).unwrap_or(0);

        let mut datagrams = Vec::new();
        for receiver in routing.receivers(session) {
            let Some(peer) = self.peers.by_session(*receiver) else {
                // In the table but no longer connected: it disconnected between
                // the shard's last render and this packet.
                continue;
            };
            if peer.cursor() < since {
                // It has not been told this speaker exists. The client would
                // discard the audio anyway, so sending it would only waste an
                // encryption.
                continue;
            }
            self.deliver(&peer, audio, session, &mut datagrams);
        }
        datagrams
    }

    /// The client asked the server to send its own voice back.
    ///
    /// Not a route: the audio relation deliberately never contains a self edge,
    /// because echoing a speaker back to itself is the classic doubled-voice
    /// bug. This is a separate mechanism the client explicitly asks for, and it
    /// stays because it is how a human checks a fresh deployment end to end.
    ///
    /// Going through no route means it has to ask the mute question itself.
    /// Deafness is deliberately not asked: the reference server tests it when
    /// adding a *receiver*, and the loopback skips that path entirely, so a
    /// deafened speaker still hears its own echo.
    ///
    /// REF: references/mumble/src/murmur/Server.cpp : `processMsg` returns on
    ///   `bMute || bSuppress || bSelfMute` before it reaches the
    ///   `SERVER_LOOPBACK` branch.
    /// REF: references/mumble/src/murmur/AudioReceiverBuffer.cpp : the loopback
    ///   goes through `forceAddReceiver`, which does not test `bDeaf`.
    fn reflect(&self, sender: &Arc<Peer>, audio: &udp::Audio) -> Vec<Datagram> {
        let session = sender.session();
        if !sender.routing().may_speak(session) {
            eprintln!(
                "mumble-server-runtime-gateway: session {session:?}: refusing loopback, this speaker is muted"
            );
            return Vec::new();
        }

        let mut datagrams = Vec::new();
        self.deliver(sender, audio, session, &mut datagrams);
        datagrams
    }

    /// Seal one packet for one receiver, on whichever transport that receiver
    /// last used.
    ///
    /// Cross-transport delivery needs no special case: the sender's transport
    /// never enters into it.
    fn deliver(
        &self,
        receiver: &Arc<Peer>,
        audio: &udp::Audio,
        sender: mumble_server_runtime_shard::SessionId,
        out: &mut Vec<Datagram>,
    ) {
        let plaintext = encode_udp(&UdpMessage::Audio(outgoing(audio, sender)));

        match receiver.destination() {
            Some(address) => match receiver.encrypt(&plaintext) {
                Some(sealed) => out.push((sealed, address)),
                None => eprintln!(
                    "mumble-server-runtime-gateway: dropping audio for session {:?}: no usable crypto state",
                    receiver.session()
                ),
            },
            None => match receiver
                .queue()
                .push_voice(ControlMessage::UdpTunnel(plaintext))
            {
                VoiceAdmission::Accepted => {}
                // A gap is the right outcome for a receiver already behind:
                // stale voice helps nobody, and refusing it here keeps the same
                // connection healthy for the control traffic that still matters.
                VoiceAdmission::Dropped => {}
            },
        }
    }

    /// A `Ping` reply echoing the client's timestamp plus server statistics.
    fn ping_reply(&self, request: &udp::Ping) -> Vec<u8> {
        encode_udp(&UdpMessage::Ping(udp::Ping {
            timestamp: request.timestamp,
            request_extended_information: false,
            server_version_v2: self.config.version_v2(),
            user_count: u32::try_from(self.peers.len()).unwrap_or(u32::MAX),
            max_user_count: self.config.max_users,
            max_bandwidth_per_user: self.config.max_bandwidth,
        }))
    }
}

/// Rewrite a client-sent packet into its server-to-client form.
///
/// The Opus payload travels untouched: decoding it would only be needed for
/// mixing, transcoding or content analysis, none of which happen here.
///
/// REF: references/vendored/MumbleUDP.proto : the `Header` oneof carries
///   `target` client-to-server and `context` server-to-client, so the target is
///   replaced rather than forwarded; `sender_session` "will always be set when
///   receiving audio from the server".
fn outgoing(source: &udp::Audio, sender: mumble_server_runtime_shard::SessionId) -> udp::Audio {
    udp::Audio {
        header: Some(udp::audio::Header::Context(NORMAL_CONTEXT)),
        sender_session: sender.0,
        frame_number: source.frame_number,
        opus_data: source.opus_data.clone(),
        // Positional audio is a flavor concern this build does not express, and
        // forwarding coordinates a flavor never authorised would leak position.
        positional_data: Vec::new(),
        volume_adjustment: 0.0,
        is_terminator: source.is_terminator,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn the_outgoing_envelope_replaces_the_target_with_a_context() {
        let source = udp::Audio {
            header: Some(udp::audio::Header::Target(0)),
            sender_session: 0,
            frame_number: 42,
            opus_data: vec![1, 2, 3],
            positional_data: vec![1.0, 2.0, 3.0],
            volume_adjustment: 0.0,
            is_terminator: false,
        };

        let rewritten = outgoing(&source, mumble_server_runtime_shard::SessionId(9));

        assert_eq!(
            rewritten.header,
            Some(udp::audio::Header::Context(NORMAL_CONTEXT))
        );
        assert_eq!(rewritten.sender_session, 9);
        assert_eq!(rewritten.frame_number, 42);
        assert_eq!(
            rewritten.opus_data, source.opus_data,
            "routing must never decode Opus"
        );
        assert!(
            rewritten.positional_data.is_empty(),
            "coordinates no flavor authorised must not travel"
        );
    }

    #[test]
    fn a_generated_crypt_setup_round_trips_through_its_own_state() {
        let rng = SystemRandom::new();
        let (setup, mut server) = generate_crypt_setup(&rng).expect("randomness");

        let key: [u8; KEY_SIZE] = setup
            .key
            .expect("a key")
            .try_into()
            .expect("the right length");
        let server_nonce: [u8; BLOCK_SIZE] = setup
            .server_nonce
            .expect("a nonce")
            .try_into()
            .expect("the right length");
        let client_nonce: [u8; BLOCK_SIZE] = setup
            .client_nonce
            .expect("a nonce")
            .try_into()
            .expect("the right length");

        // The client mirrors the two nonces: what the server encrypts with, the
        // client decrypts with.
        let mut client = CryptState::new(&key, &client_nonce, &server_nonce);
        let sealed = server.encrypt(b"a voice packet").expect("encryptable");

        assert_eq!(
            client.decrypt(&sealed).as_deref(),
            Some(&b"a voice packet"[..])
        );
    }
}
