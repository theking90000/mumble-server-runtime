//! The UDP voice plane (Phase 3): crypto setup, address association by proof,
//! ping replies and loopback reflection.
//!
//! Only what P3 needs: a client that has completed the TCP handshake (and thus
//! holds the OCB2 key) can prove ownership of a UDP address by sending any packet
//! that decrypts, after which the server reflects its loopback audio (target 31)
//! back to it. Real cross-user routing is P4.
//!
//! REF: references/mumble/src/murmur/Server.cpp : `Server::run` / `checkDecrypt`
//!   — an unknown peer is bound to the first session whose OCB2 state decrypts
//!   its datagram; a failed decrypt is side-effect-free (IV restored), so trying
//!   several candidates is safe (verified in P2, see docs/STATUS.md).
//! REF: references/vendored/MumbleUDP.proto : Audio target 31 = "server loopback".

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use ring::rand::{SecureRandom, SystemRandom};
use tokio::net::UdpSocket;
use voxloom_crypto::{BLOCK_SIZE, CryptState, KEY_SIZE};
use voxloom_protocol::messages::{tcp, udp};
use voxloom_protocol::{UdpMessage, decode_udp, encode_udp};

use crate::state::{SharedState, UserEntry};

/// The reserved Audio target meaning "server loopback".
/// REF: references/vendored/MumbleUDP.proto : `target ... 2^{5} - 1 means "server loopback"`.
pub const LOOPBACK_TARGET: u32 = 31;

/// Generate a fresh OCB2 key and the two nonces for a new connection, returning
/// both the `CryptSetup` message to send and the server's `CryptState`.
///
/// REF: references/mumble/src/murmur/Messages.cpp : `msgAuthenticate` sets
///   `server_nonce = getEncryptIV()`, `client_nonce = getDecryptIV()`. So on the
///   server the encrypt IV is the server nonce (S2C) and the decrypt IV is the
///   client nonce (C2S).
pub fn generate_crypt_setup(rng: &SystemRandom) -> Result<(tcp::CryptSetup, CryptState)> {
    let mut key = [0u8; KEY_SIZE];
    let mut server_nonce = [0u8; BLOCK_SIZE];
    let mut client_nonce = [0u8; BLOCK_SIZE];
    rng.fill(&mut key)
        .map_err(|_| anyhow::anyhow!("rng failed for OCB2 key"))?;
    rng.fill(&mut server_nonce)
        .map_err(|_| anyhow::anyhow!("rng failed for server nonce"))?;
    rng.fill(&mut client_nonce)
        .map_err(|_| anyhow::anyhow!("rng failed for client nonce"))?;

    let state = CryptState::new(&key, &server_nonce, &client_nonce);
    let setup = tcp::CryptSetup {
        key: Some(key.to_vec()),
        server_nonce: Some(server_nonce.to_vec()),
        client_nonce: Some(client_nonce.to_vec()),
    };
    Ok((setup, state))
}

/// The UDP voice plane task.
pub struct VoicePlane {
    socket: Arc<UdpSocket>,
    state: Arc<SharedState>,
}

impl VoicePlane {
    pub fn new(socket: Arc<UdpSocket>, state: Arc<SharedState>) -> Self {
        Self { socket, state }
    }

    /// Receive and service datagrams until the socket errors. Runs for the life
    /// of the server.
    pub async fn run(self) -> Result<()> {
        // Max Mumble datagram; larger reads are truncated by recv_from anyway.
        let mut buf = vec![0u8; 2048];
        loop {
            let (len, addr) = self
                .socket
                .recv_from(&mut buf)
                .await
                .context("UDP recv_from")?;
            let datagram = &buf[..len];
            // Every datagram ends in an explicit outcome (L4): a reply, a binding,
            // or a logged drop. Errors handling one datagram never kill the plane.
            for (reply, to) in self.handle_datagram(datagram, addr) {
                if let Err(error) = self.socket.send_to(&reply, to).await {
                    // A single failed send is not fatal to the plane.
                    eprintln!("voxloom-server: UDP send to {to} failed: {error}");
                }
            }
        }
    }

    /// Process one datagram, returning any datagrams to send back. All crypto is
    /// synchronous and done here; the caller performs the awaited sends.
    fn handle_datagram(&self, datagram: &[u8], addr: SocketAddr) -> Vec<(Vec<u8>, SocketAddr)> {
        // 1. A peer we have already bound: use its session directly.
        if let Some(user) = self.state.user_for_addr(&addr) {
            return self.handle_from_known(&user, datagram, addr);
        }

        // 2. Unknown peer: try to prove ownership against each user's crypto.
        for user in self.state.users() {
            if let Some(plaintext) = try_decrypt(&user, datagram) {
                self.state.bind_udp(addr, user.session);
                return self.reply_to_plaintext(&user, &plaintext, addr);
            }
        }

        // 3. Not encrypted for anyone: maybe an unencrypted connectivity ping.
        if let Ok(UdpMessage::Ping(ping)) = decode_udp(datagram) {
            return vec![(self.build_ping_reply(&ping), addr)];
        }

        // 4. Nothing matched: fail closed with a logged drop (L4).
        eprintln!("voxloom-server: dropping unroutable UDP datagram from {addr}");
        Vec::new()
    }

    /// Handle a datagram from an address already bound to `user`.
    fn handle_from_known(
        &self,
        user: &Arc<UserEntry>,
        datagram: &[u8],
        addr: SocketAddr,
    ) -> Vec<(Vec<u8>, SocketAddr)> {
        match try_decrypt(user, datagram) {
            Some(plaintext) => self.reply_to_plaintext(user, &plaintext, addr),
            None => {
                // A bound peer whose packet no longer decrypts: replay, tamper or
                // desync. Drop it (logged); resync handling is future work.
                eprintln!(
                    "voxloom-server: undecryptable UDP from bound session {} at {addr}",
                    user.session
                );
                Vec::new()
            }
        }
    }

    /// Decode a decrypted voice-plane packet and produce any reply.
    fn reply_to_plaintext(
        &self,
        user: &Arc<UserEntry>,
        plaintext: &[u8],
        addr: SocketAddr,
    ) -> Vec<(Vec<u8>, SocketAddr)> {
        match decode_udp(plaintext) {
            Ok(UdpMessage::Ping(ping)) => {
                let reply = self.build_ping_reply(&ping);
                match encrypt(user, &reply) {
                    Some(sealed) => vec![(sealed, addr)],
                    None => Vec::new(),
                }
            }
            Ok(UdpMessage::Audio(audio)) => self.reflect_loopback(user, audio, addr),
            Err(error) => {
                eprintln!(
                    "voxloom-server: bad UDP envelope from session {}: {error}",
                    user.session
                );
                Vec::new()
            }
        }
    }

    /// Reflect a loopback audio packet (target 31) back to its sender. Any other
    /// target has no route in P3 and is dropped (real routing is P4).
    fn reflect_loopback(
        &self,
        user: &Arc<UserEntry>,
        audio: udp::Audio,
        addr: SocketAddr,
    ) -> Vec<(Vec<u8>, SocketAddr)> {
        let target = match audio.header {
            Some(udp::audio::Header::Target(target)) => target,
            // Server->client audio uses `context`; a client should never send it.
            _ => {
                return Vec::new();
            }
        };
        if target != LOOPBACK_TARGET {
            // Normal talking (target 0) and whisper/shout targets need the audio
            // routing graph, which is P4. Drop quietly.
            return Vec::new();
        }

        let reflected = server_audio_for_tunnel(&audio, user.session);
        let plaintext = encode_udp(&UdpMessage::Audio(reflected));
        match encrypt(user, &plaintext) {
            Some(sealed) => vec![(sealed, addr)],
            None => Vec::new(),
        }
    }

    /// Build a UDP `Ping` reply echoing the client timestamp plus server stats.
    fn build_ping_reply(&self, request: &udp::Ping) -> Vec<u8> {
        let config = self.state.config();
        let reply = udp::Ping {
            timestamp: request.timestamp,
            request_extended_information: false,
            server_version_v2: config.version_v2(),
            user_count: u32::try_from(self.state.users().len()).unwrap_or(u32::MAX),
            max_user_count: config.max_users,
            max_bandwidth_per_user: config.max_bandwidth,
        };
        encode_udp(&UdpMessage::Ping(reply))
    }
}

/// Turn a client-sent Audio packet into the server->client form: drop the target
/// header, mark the context as normal speech, and stamp the sender session so the
/// client can attribute the stream (§14.1). The Opus payload is untouched (§15.2).
/// Shared by the UDP plane and the TCP tunnel fallback (`connection.rs`).
pub fn server_audio_for_tunnel(source: &udp::Audio, sender_session: u32) -> udp::Audio {
    udp::Audio {
        header: Some(udp::audio::Header::Context(0)),
        sender_session,
        frame_number: source.frame_number,
        opus_data: source.opus_data.clone(),
        positional_data: source.positional_data.clone(),
        volume_adjustment: 0.0,
        is_terminator: source.is_terminator,
    }
}

/// Try to decrypt a datagram against a user's OCB2 state. `None` if the user has
/// no crypto yet or the packet does not authenticate (a failure is side-effect
/// free — the IV is restored — so the caller may try other users safely).
fn try_decrypt(user: &UserEntry, datagram: &[u8]) -> Option<Vec<u8>> {
    let mut guard = user.crypto.lock().ok()?;
    guard.as_mut()?.decrypt(datagram)
}

/// Encrypt a plaintext voice packet with a user's OCB2 state.
fn encrypt(user: &UserEntry, plaintext: &[u8]) -> Option<Vec<u8>> {
    let mut guard = user.crypto.lock().ok()?;
    guard.as_mut()?.encrypt(plaintext)
}
