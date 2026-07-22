//! Decode a captured `.voxcap` session into a readable, byte-complete transcript.
//!
//! This is the Phase 1 done-criterion: it replays real captured traffic through
//! the pure codec ([`voxloom_protocol`]) and crypto ([`voxloom_crypto`]) and
//! accounts for every byte. It is a verification tool, not part of the runtime,
//! so it lives under `tools/` with IO allowed (R4/R5); the pure crates never
//! depend back on it.
//!
//! Two things the corpus taught us that the task's framing did not anticipate,
//! both verified against the vendored reference (R1), never guessed:
//!
//! 1. Not all UDP is OCB2-encrypted. Before the crypt handshake, the client and
//!    server exchange **unencrypted legacy connectivity pings** (the server-list
//!    style ping). REF: references/mumble/src/MumbleProtocol.cpp :
//!    `UDPDecoder::decodePing_legacy` (12-byte request with four leading zero
//!    bytes + 64-bit timestamp; 24-byte response with legacy version, timestamp,
//!    user/max/bandwidth counts). The Mumble server tries this ping decode BEFORE
//!    attempting decryption. REF: references/mumble/src/murmur/Server.cpp :
//!    `Server::run` (`decodePing` then `checkDecrypt`).
//!
//! 2. The corpus server is Mumble 1.3.4, which predates protobuf UDP
//!    (introduced in 1.5.0). REF: references/mumble/src/MumbleProtocol.h :
//!    `PROTOBUF_INTRODUCTION_VERSION = 1.5.0`. Its encrypted UDP voice plane is
//!    therefore the **legacy** wire format, which ADR-0001 deliberately excludes
//!    from `voxloom-protocol`. OCB2 decryption (version-independent) still works;
//!    `decode_udp` (protobuf-only) correctly rejects the legacy plaintext. The
//!    transcript reports such packets as decrypted-but-legacy rather than
//!    pretending to structurally decode a format the codec does not support.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};

use voxloom_crypto::{BLOCK_SIZE, CryptState, KEY_SIZE};
use voxloom_protocol::{ControlMessage, decode_control, decode_udp, parse_frame};
// Re-exported: `Decoded::Udp` wraps this, so a consumer matching on the
// transcript needs the type without depending on voxloom-protocol directly.
pub use voxloom_protocol::UdpMessage;

/// Magic header of a `.voxcap` file, format version 01.
/// REF: tools/recording-proxy/src/capture.rs : `MAGIC` (the format's authority).
const MAGIC: &[u8; 8] = b"VOXCAP01";

/// Fixed size of a record header: `[dir:u8][transport:u8][ts:i64 LE][len:u32 LE]`.
/// REF: tools/recording-proxy/src/capture.rs : `write_record` / `read_records`.
const RECORD_HEADER_LEN: usize = 1 + 1 + 8 + 4;

/// Same hard cap the writer enforces, so a corrupt length never drives an
/// allocation. REF: capture.rs : `MAX_RECORD_LEN`.
const MAX_RECORD_LEN: u32 = 16 * 1024 * 1024;

/// Direction of a captured record relative to the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    ClientToServer,
    ServerToClient,
}

impl Dir {
    fn from_code(code: u8) -> Result<Self> {
        match code {
            0 => Ok(Dir::ClientToServer),
            1 => Ok(Dir::ServerToClient),
            other => Err(anyhow!("invalid direction code {other}")),
        }
    }

    /// Short label used in the transcript.
    pub fn label(self) -> &'static str {
        match self {
            Dir::ClientToServer => "c2s",
            Dir::ServerToClient => "s2c",
        }
    }
}

/// Transport a captured record travelled over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Tcp,
    Udp,
}

impl Transport {
    fn from_code(code: u8) -> Result<Self> {
        match code {
            0 => Ok(Transport::Tcp),
            1 => Ok(Transport::Udp),
            other => Err(anyhow!("invalid transport code {other}")),
        }
    }
}

/// One captured record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub dir: Dir,
    pub transport: Transport,
    pub ts_micros: i64,
    pub data: Vec<u8>,
}

/// Read every record from a `.voxcap` file.
///
/// This re-implements the reader that lives in the recording proxy's
/// `capture.rs`; the format is frozen (`VOXCAP01`) and re-reading it here keeps
/// this tool from depending on the proxy binary crate. If the format ever
/// changes, the magic bumps and both readers must move together.
pub fn read_records(path: impl AsRef<Path>) -> Result<Vec<Record>> {
    let path = path.as_ref();
    let bytes =
        std::fs::read(path).with_context(|| format!("reading capture file {}", path.display()))?;

    let body = match bytes.get(..MAGIC.len()) {
        Some(magic) if magic == MAGIC => &bytes[MAGIC.len()..],
        Some(magic) => bail!(
            "bad magic in {}: expected {:?}, found {:?}",
            path.display(),
            MAGIC,
            magic
        ),
        None => bail!("{} is too short to hold the magic header", path.display()),
    };

    let mut records = Vec::new();
    let mut offset = 0usize;
    while offset < body.len() {
        let header = body
            .get(offset..offset + RECORD_HEADER_LEN)
            .ok_or_else(|| {
                anyhow!(
                    "truncated record header at byte {offset} in {}",
                    path.display()
                )
            })?;

        let dir = Dir::from_code(header[0])?;
        let transport = Transport::from_code(header[1])?;
        let mut ts_bytes = [0u8; 8];
        ts_bytes.copy_from_slice(&header[2..10]);
        let ts_micros = i64::from_le_bytes(ts_bytes);
        let mut len_bytes = [0u8; 4];
        len_bytes.copy_from_slice(&header[10..14]);
        let declared = u32::from_le_bytes(len_bytes);
        if declared > MAX_RECORD_LEN {
            bail!(
                "record length {declared} at byte {offset} in {} exceeds maximum {MAX_RECORD_LEN}",
                path.display()
            );
        }
        let len = usize::try_from(declared).context("record length exceeds usize")?;

        let data_start = offset + RECORD_HEADER_LEN;
        let data = body
            .get(data_start..data_start + len)
            .ok_or_else(|| {
                anyhow!(
                    "truncated record body at byte {data_start} in {}",
                    path.display()
                )
            })?
            .to_vec();

        records.push(Record {
            dir,
            transport,
            ts_micros,
            data,
        });
        offset = data_start + len;
    }

    Ok(records)
}

/// The parsed contents of a single UDP legacy connectivity ping.
/// REF: MumbleProtocol.cpp : `UDPDecoder::decodePing_legacy`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyPing {
    /// 12-byte request: four zero bytes then a 64-bit client timestamp (opaque).
    Request { timestamp: u64 },
    /// 24-byte response: legacy server version, echoed timestamp, and counts.
    Response {
        version: u32,
        timestamp: u64,
        user_count: u32,
        max_user_count: u32,
        max_bandwidth_per_user: u32,
    },
}

/// Which legacy UDP payload a decrypted packet turned out to be, classified by
/// the header byte's top three bits.
/// REF: MumbleProtocol.cpp : `UDPDecoder::decode` legacy branch
/// (`(header >> 5) & 0x7`) and `LegacyUDPMessageType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyUdpKind {
    Ping,
    VoiceCeltAlpha,
    VoiceCeltBeta,
    VoiceSpeex,
    VoiceOpus,
    Unknown(u8),
}

impl LegacyUdpKind {
    fn from_header(header: u8) -> Self {
        // REF: MumbleProtocol.cpp : `(header >> 5) & 0x7` -> LegacyUDPMessageType.
        match (header >> 5) & 0x7 {
            1 => LegacyUdpKind::Ping,
            0 => LegacyUdpKind::VoiceCeltAlpha,
            2 => LegacyUdpKind::VoiceCeltBeta,
            3 => LegacyUdpKind::VoiceSpeex,
            4 => LegacyUdpKind::VoiceOpus,
            other => LegacyUdpKind::Unknown(other),
        }
    }

    pub fn label(self) -> String {
        match self {
            LegacyUdpKind::Ping => "legacy Ping".to_string(),
            LegacyUdpKind::VoiceCeltAlpha => "legacy voice (CELT Alpha)".to_string(),
            LegacyUdpKind::VoiceCeltBeta => "legacy voice (CELT Beta)".to_string(),
            LegacyUdpKind::VoiceSpeex => "legacy voice (Speex)".to_string(),
            LegacyUdpKind::VoiceOpus => "legacy voice (Opus)".to_string(),
            LegacyUdpKind::Unknown(code) => format!("legacy unknown type {code}"),
        }
    }
}

/// What a single record decoded to. Every arm accounts for the record's bytes;
/// none is a silent drop (R6).
#[derive(Debug, Clone, PartialEq)]
pub enum Decoded {
    /// A framed TCP control message. Boxed: the protobuf message structs dwarf
    /// the other variants, so indirection keeps `Decoded` small.
    Control(Box<ControlMessage>),
    /// An unencrypted UDP connectivity ping (pre-crypt, legacy format).
    LegacyConnectivityPing(LegacyPing),
    /// A decrypted UDP packet whose plaintext is a protobuf envelope (Mumble
    /// 1.5+). Produced by scenarios 03-07, whose server is 1.5.857 (scenarios
    /// 01-02 hit a 1.3.4 server and yield `DecryptedLegacyUdp` instead). Boxed
    /// for the same reason as `Control`.
    Udp(Box<UdpMessage>),
    /// A decrypted UDP packet whose plaintext is the legacy wire format
    /// (ADR-0001: out of scope for `voxloom-protocol`). Decryption succeeded;
    /// the envelope is reported, not structurally decoded.
    DecryptedLegacyUdp {
        kind: LegacyUdpKind,
        plaintext_len: usize,
    },
    /// An encrypted UDP packet OCB2 rejected (replay, out-of-window late packet,
    /// or tag mismatch). This is a definite, accounted-for outcome, not an
    /// unexplained byte: the real Murmur server drops exactly these the same way
    /// (`checkDecrypt` returns false and the packet is skipped).
    /// REF: murmur/Server.cpp : `Server::checkDecrypt` (false -> `continue`).
    RejectedUdp { len: usize },
}

/// One transcript line: a decoded record with its timeline metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub ts_micros: i64,
    pub dir: Dir,
    pub transport: Transport,
    pub decoded: Decoded,
}

/// The full decode of a session, plus the totals that let a caller assert the
/// "every byte accounted for" property.
#[derive(Debug, Default)]
pub struct Transcript {
    pub events: Vec<Event>,
    pub tcp_frames: usize,
    pub udp_packets: usize,
    /// UDP packets OCB2 rejected (replay / late / tag). Expected to be a small
    /// tail of any real capture; a large share signals a decode bug, not drops.
    pub udp_rejected: usize,
    /// Bytes of TCP stream that remained unparsed at end of session, per
    /// direction. A clean session ends on a frame boundary: both are zero.
    pub tcp_trailing_c2s: usize,
    pub tcp_trailing_s2c: usize,
}

/// Reassemble the TCP control stream in one direction from record chunks and
/// drain complete frames. The recording captures stream bytes, not aligned
/// messages, so frames straddle record boundaries and are parsed incrementally.
/// REF: framing.rs : `parse_frame` (incremental; `Ok(None)` means "need more").
struct TcpStream {
    buffer: Vec<u8>,
}

impl TcpStream {
    fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    /// Append a chunk and drain every complete frame, decoding each to a
    /// [`ControlMessage`]. Returns the decoded messages in wire order.
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<ControlMessage>> {
        self.buffer.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            let (message_type, payload, total_len) =
                match parse_frame(&self.buffer).context("framing the TCP control stream")? {
                    Some(frame) => (
                        frame.message_type,
                        frame.payload.to_vec(),
                        frame.total_len(),
                    ),
                    None => break,
                };
            let message = decode_control(message_type, &payload).with_context(|| {
                format!(
                    "decoding control message type {message_type} ({} bytes)",
                    payload.len()
                )
            })?;
            out.push(message);
            self.buffer.drain(..total_len);
        }
        Ok(out)
    }
}

/// Per-direction OCB2 state, built from the `CryptSetup` seen on the control
/// channel. The client encrypts C2S with `client_nonce` and decrypts S2C with
/// `server_nonce`; the server is symmetric. So to decrypt a C2S datagram we run
/// a `CryptState` whose decrypt IV is `client_nonce`, and for S2C, `server_nonce`.
///
/// REF: references/mumble/src/mumble/Messages.cpp : `MainWindow::msgCryptSetup`
///      -> `csCrypt->setKey(key, client_nonce, server_nonce)`, where
///      `setKey(rkey, eiv, div)` sets encrypt_iv=client_nonce, decrypt_iv=server_nonce.
/// REF: references/mumble/src/murmur/Messages.cpp : the server fills
///      `server_nonce = getEncryptIV()`, `client_nonce = getDecryptIV()` (symmetric).
struct CryptContext {
    c2s: CryptState,
    s2c: CryptState,
}

impl CryptContext {
    /// Build from a `CryptSetup` carrying key, client_nonce and server_nonce.
    fn from_setup(key: &[u8], client_nonce: &[u8], server_nonce: &[u8]) -> Result<Self> {
        let key: [u8; KEY_SIZE] = key
            .try_into()
            .map_err(|_| anyhow!("CryptSetup key is {} bytes, expected {KEY_SIZE}", key.len()))?;
        let client_nonce: [u8; BLOCK_SIZE] = client_nonce.try_into().map_err(|_| {
            anyhow!(
                "CryptSetup client_nonce is {} bytes, expected {BLOCK_SIZE}",
                client_nonce.len()
            )
        })?;
        let server_nonce: [u8; BLOCK_SIZE] = server_nonce.try_into().map_err(|_| {
            anyhow!(
                "CryptSetup server_nonce is {} bytes, expected {BLOCK_SIZE}",
                server_nonce.len()
            )
        })?;

        // The encrypt IV is irrelevant here (we only decrypt); pass the matching
        // nonce so each state mirrors one real endpoint exactly.
        Ok(Self {
            c2s: CryptState::new(&key, &server_nonce, &client_nonce),
            s2c: CryptState::new(&key, &client_nonce, &server_nonce),
        })
    }

    fn decrypt(&mut self, dir: Dir, packet: &[u8]) -> Option<Vec<u8>> {
        match dir {
            Dir::ClientToServer => self.c2s.decrypt(packet),
            Dir::ServerToClient => self.s2c.decrypt(packet),
        }
    }

    /// Resync the S2C decrypt IV from a nonce-only CryptSetup (server -> client).
    fn resync_s2c(&mut self, server_nonce: &[u8]) -> Result<()> {
        let iv: [u8; BLOCK_SIZE] = server_nonce.try_into().map_err(|_| {
            anyhow!(
                "resync server_nonce is {} bytes, expected {BLOCK_SIZE}",
                server_nonce.len()
            )
        })?;
        self.s2c.set_decrypt_iv(&iv);
        Ok(())
    }

    /// Resync the C2S decrypt IV from a nonce-only CryptSetup (client -> server).
    fn resync_c2s(&mut self, client_nonce: &[u8]) -> Result<()> {
        let iv: [u8; BLOCK_SIZE] = client_nonce.try_into().map_err(|_| {
            anyhow!(
                "resync client_nonce is {} bytes, expected {BLOCK_SIZE}",
                client_nonce.len()
            )
        })?;
        self.c2s.set_decrypt_iv(&iv);
        Ok(())
    }
}

/// Decode a whole session into a transcript, accounting for every record.
///
/// Records are processed in capture order. TCP records feed two per-direction
/// reassembly buffers; UDP records are classified (unencrypted connectivity ping
/// before crypt setup, OCB2-encrypted after) and decoded or decrypted.
pub fn decode_session(records: &[Record]) -> Result<Transcript> {
    let mut transcript = Transcript::default();
    let mut c2s = TcpStream::new();
    let mut s2c = TcpStream::new();
    let mut crypt: Option<CryptContext> = None;

    for record in records {
        match record.transport {
            Transport::Tcp => {
                let stream = match record.dir {
                    Dir::ClientToServer => &mut c2s,
                    Dir::ServerToClient => &mut s2c,
                };
                let messages = stream.push(&record.data).with_context(|| {
                    format!(
                        "in {} TCP stream at ts {}",
                        record.dir.label(),
                        record.ts_micros
                    )
                })?;
                for message in messages {
                    // Adopt the crypt keys whenever the server sends a full
                    // CryptSetup (key + both nonces). This fires again on a
                    // re-key mid-session (the client's msgCryptSetup calls
                    // setKey each time all three are present), so we must rebuild,
                    // not latch only the first. A nonce-only CryptSetup is an IV
                    // resync, handled below.
                    // REF: mumble/Messages.cpp : `MainWindow::msgCryptSetup`.
                    if let ControlMessage::CryptSetup(setup) = &message {
                        if let (Some(key), Some(cn), Some(sn)) =
                            (&setup.key, &setup.client_nonce, &setup.server_nonce)
                        {
                            crypt =
                                Some(CryptContext::from_setup(key, cn, sn).with_context(|| {
                                    format!(
                                        "building crypt state from CryptSetup at ts {}",
                                        record.ts_micros
                                    )
                                })?);
                        } else if let Some(context) = crypt.as_mut() {
                            // Nonce-only resync of one direction's decrypt IV: the
                            // server sends server_nonce to resync S2C, the client
                            // sends client_nonce to resync C2S.
                            // REF: Messages.cpp : `setDecryptIV(server_nonce)`.
                            if let Some(sn) = &setup.server_nonce {
                                context.resync_s2c(sn).with_context(|| {
                                    format!("applying S2C nonce resync at ts {}", record.ts_micros)
                                })?;
                            }
                            if let Some(cn) = &setup.client_nonce {
                                context.resync_c2s(cn).with_context(|| {
                                    format!("applying C2S nonce resync at ts {}", record.ts_micros)
                                })?;
                            }
                        }
                    }
                    transcript.tcp_frames += 1;
                    transcript.events.push(Event {
                        ts_micros: record.ts_micros,
                        dir: record.dir,
                        transport: Transport::Tcp,
                        decoded: Decoded::Control(Box::new(message)),
                    });
                }
            }
            Transport::Udp => {
                transcript.udp_packets += 1;
                let decoded = decode_udp_record(record, crypt.as_mut()).with_context(|| {
                    format!(
                        "decoding {} UDP packet at ts {}",
                        record.dir.label(),
                        record.ts_micros
                    )
                })?;
                if matches!(decoded, Decoded::RejectedUdp { .. }) {
                    transcript.udp_rejected += 1;
                }
                transcript.events.push(Event {
                    ts_micros: record.ts_micros,
                    dir: record.dir,
                    transport: Transport::Udp,
                    decoded,
                });
            }
        }
    }

    transcript.tcp_trailing_c2s = c2s.buffer.len();
    transcript.tcp_trailing_s2c = s2c.buffer.len();
    Ok(transcript)
}

/// Classify and decode a single UDP datagram, failing closed (R6): a packet we
/// cannot explain is an error, never a silent drop.
fn decode_udp_record(record: &Record, crypt: Option<&mut CryptContext>) -> Result<Decoded> {
    // An unencrypted ping can appear both before crypt setup (a probing client)
    // and after it (a periodic server-browser ping). Mumble decodes pings on the
    // raw bytes BEFORE ever attempting decryption, so try that first in both
    // cases. REF: murmur/Server.cpp : `decodePing` then `checkDecrypt`.
    if let Some(ping) = decode_unencrypted_ping(record) {
        return Ok(ping);
    }

    match crypt {
        // No crypt yet and not a recognisable ping: unexplained -> error (R6).
        None => Err(anyhow!(
            "unexplained {}-byte UDP packet before CryptSetup \
             (not a legacy connectivity ping, and not a protobuf ping)",
            record.data.len()
        )),
        Some(context) => {
            let plaintext = match context.decrypt(record.dir, &record.data) {
                Some(plaintext) => plaintext,
                // OCB2 rejected it. A definite, accounted-for outcome (the real
                // server drops these identically), not an unexplained byte.
                None => {
                    return Ok(Decoded::RejectedUdp {
                        len: record.data.len(),
                    });
                }
            };
            // Decrypted. Protobuf envelope (1.5+) or legacy (<1.5)?
            match decode_udp(&plaintext) {
                Ok(message) => Ok(Decoded::Udp(Box::new(message))),
                Err(_) => {
                    let header = plaintext.first().copied().unwrap_or(0);
                    Ok(Decoded::DecryptedLegacyUdp {
                        kind: LegacyUdpKind::from_header(header),
                        plaintext_len: plaintext.len(),
                    })
                }
            }
        }
    }
}

/// Recognise an unencrypted UDP ping in either wire format, or return `None` if
/// the bytes are not one. A 1.5 client probing a server of unknown version emits
/// both a legacy connectivity ping (12/24-byte fixed shape) and a protobuf ping
/// (header `0x01` + `MumbleUDP.Ping`).
///
/// The protobuf arm is gated on header byte `0x01` (Ping) so an encrypted packet
/// whose IV byte happens to be `0x00` (Audio) is never mistaken for a plaintext
/// ping; requiring a clean protobuf parse of a `Ping` is the same guard Mumble
/// relies on. REF: MumbleProtocol.cpp : `UDPDecoder::decode` (protobuf-ping when
/// `header == UDPMessageType::Ping`, else the legacy shapes).
fn decode_unencrypted_ping(record: &Record) -> Option<Decoded> {
    if let Some(ping) = decode_legacy_connectivity_ping(record) {
        return Some(ping);
    }
    if record.data.first() == Some(&1)
        && let Ok(message @ UdpMessage::Ping(_)) = decode_udp(&record.data)
    {
        return Some(Decoded::Udp(Box::new(message)));
    }
    None
}

/// Recognise an unencrypted legacy connectivity ping by its fixed shapes.
/// Returns `None` if the bytes are not one of those shapes.
/// REF: MumbleProtocol.cpp : `decodePing_legacy` (client 24-byte response,
///      server 12-byte request with four leading zero bytes).
fn decode_legacy_connectivity_ping(record: &Record) -> Option<Decoded> {
    match (record.dir, record.data.len()) {
        // 12-byte request: [0,0,0,0][u64 timestamp]. The four zero bytes are the
        // legacy "extended information request" marker.
        (Dir::ClientToServer, 12) if record.data[..4] == [0, 0, 0, 0] => {
            let timestamp = u64::from_be_bytes(record.data[4..12].try_into().ok()?);
            Some(Decoded::LegacyConnectivityPing(LegacyPing::Request {
                timestamp,
            }))
        }
        // 24-byte response: six big-endian u32 fields; entries 1..2 form the
        // echoed 64-bit timestamp (opaque, byte order unspecified).
        (Dir::ServerToClient, 24) => {
            let word = |i: usize| -> Option<u32> {
                let start = i * 4;
                Some(u32::from_be_bytes(
                    record.data.get(start..start + 4)?.try_into().ok()?,
                ))
            };
            let timestamp = u64::from_be_bytes(record.data.get(4..12)?.try_into().ok()?);
            Some(Decoded::LegacyConnectivityPing(LegacyPing::Response {
                version: word(0)?,
                timestamp,
                user_count: word(3)?,
                max_user_count: word(4)?,
                max_bandwidth_per_user: word(5)?,
            }))
        }
        _ => None,
    }
}

/// Render a `ControlMessage`'s type name for the transcript. Field-level detail
/// is left to the message's `Debug`; this keeps the one-line summary readable.
pub fn control_type_name(message: &ControlMessage) -> &'static str {
    match message {
        ControlMessage::Version(_) => "Version",
        ControlMessage::UdpTunnel(_) => "UDPTunnel(raw audio)",
        ControlMessage::Authenticate(_) => "Authenticate",
        ControlMessage::Ping(_) => "Ping",
        ControlMessage::Reject(_) => "Reject",
        ControlMessage::ServerSync(_) => "ServerSync",
        ControlMessage::ChannelRemove(_) => "ChannelRemove",
        ControlMessage::ChannelState(_) => "ChannelState",
        ControlMessage::UserRemove(_) => "UserRemove",
        ControlMessage::UserState(_) => "UserState",
        ControlMessage::BanList(_) => "BanList",
        ControlMessage::TextMessage(_) => "TextMessage",
        ControlMessage::PermissionDenied(_) => "PermissionDenied",
        ControlMessage::Acl(_) => "ACL",
        ControlMessage::QueryUsers(_) => "QueryUsers",
        ControlMessage::CryptSetup(_) => "CryptSetup",
        ControlMessage::ContextActionModify(_) => "ContextActionModify",
        ControlMessage::ContextAction(_) => "ContextAction",
        ControlMessage::UserList(_) => "UserList",
        ControlMessage::VoiceTarget(_) => "VoiceTarget",
        ControlMessage::PermissionQuery(_) => "PermissionQuery",
        ControlMessage::CodecVersion(_) => "CodecVersion",
        ControlMessage::UserStats(_) => "UserStats",
        ControlMessage::RequestBlob(_) => "RequestBlob",
        ControlMessage::ServerConfig(_) => "ServerConfig",
        ControlMessage::SuggestConfig(_) => "SuggestConfig",
        ControlMessage::PluginDataTransmission(_) => "PluginDataTransmission",
    }
}
