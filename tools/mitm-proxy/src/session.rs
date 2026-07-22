//! Per-connection control-plane state machine that turns the proxy into a true
//! man in the middle of the OCB2 key exchange.
//!
//! A real Mumble session shares one AES key and a pair of nonces between client
//! and server (`CryptSetup`). To decrypt and *re-encrypt* the UDP voice plane
//! (Phase 2's oracle), the proxy must not relay that key: it holds two fully
//! independent cipher domains instead, one toward each side, and rewrites the
//! key exchange so neither peer can tell.
//!
//! Direction and nonce mapping, traced to the reference:
//!
//! REF: references/mumble/src/murmur/Messages.cpp : `Server::msgAuthenticate`
//!      crypt-setup block — the server sends `CryptSetup{key = getRawKey(),
//!      server_nonce = getEncryptIV(), client_nonce = getDecryptIV()}`. So the
//!      server encrypts server->client with its encrypt IV (= server_nonce) and
//!      decrypts client->server with its decrypt IV (= client_nonce).
//! REF: references/mumble/src/mumble/Messages.cpp : `MainWindow::msgCryptSetup`
//!      — the client calls `setKey(key, client_nonce, server_nonce)`, i.e. its
//!      encrypt IV = client_nonce and its decrypt IV = server_nonce. Exact mirror
//!      of the server.
//! REF: references/vendored/Mumble.proto : `message CryptSetup` — "Either side may
//!      request a resync by sending the message without any values filled. The
//!      resync is performed by sending the message with only the client or server
//!      nonce filled."
//! REF: references/mumble/src/murmur/Messages.cpp : `Server::msgCryptSetup` — an
//!      empty request is answered with `server_nonce = getEncryptIV()`; a message
//!      carrying `client_nonce` triggers `setDecryptIV(client_nonce)`.
//! REF: references/mumble/src/mumble/Messages.cpp : `MainWindow::msgCryptSetup` —
//!      symmetric: an empty request is answered with `client_nonce =
//!      getEncryptIV()`; a `server_nonce` alone triggers `setDecryptIV`.
//!
//! Because the two domains drift independently (UDP loss differs on each leg),
//! a nonce resync is handled *locally* at the proxy and never forwarded across
//! it: each side resyncs against the IV the proxy itself controls (fail closed,
//! L4 — an out-of-order or pre-setup resync is rejected, not guessed).

use voxloom_crypto::{BLOCK_SIZE, CryptState, KEY_SIZE};
use voxloom_protocol::ControlMessage;
use voxloom_protocol::messages::tcp;

/// The proxy's own OCB2 secrets for the client-facing domain (proxy <-> client).
///
/// These are what the rewritten `CryptSetup` hands to the real client, so the
/// client encrypts toward the proxy (not the server) and the proxy owns every
/// byte of that leg. Generated fresh per session by the binary; fixed in tests.
#[derive(Clone)]
pub struct ProxySecrets {
    key: [u8; KEY_SIZE],
    /// The proxy encrypts proxy->client with this IV.
    encrypt_iv: [u8; BLOCK_SIZE],
    /// The proxy decrypts client->proxy with this IV.
    decrypt_iv: [u8; BLOCK_SIZE],
}

impl ProxySecrets {
    pub fn new(
        key: [u8; KEY_SIZE],
        encrypt_iv: [u8; BLOCK_SIZE],
        decrypt_iv: [u8; BLOCK_SIZE],
    ) -> Self {
        Self {
            key,
            encrypt_iv,
            decrypt_iv,
        }
    }
}

// A key must never reach a log or a Debug dump.
impl std::fmt::Debug for ProxySecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxySecrets").finish_non_exhaustive()
    }
}

/// The two independent OCB2 cipher states a session owns once the server's
/// initial `CryptSetup` has been intercepted. The UDP plane (Phase 2, next
/// tranche) borrows these to decrypt on one side and re-encrypt on the other.
pub struct CryptChannels {
    /// Proxy acting as the client toward the real server (proxy <-> server).
    pub to_server: CryptState,
    /// Proxy acting as the server toward the real client (proxy <-> client).
    pub to_client: CryptState,
}

/// What the relay must do with a control message the session has inspected.
#[derive(Debug)]
pub enum Action {
    /// Forward this (possibly rewritten) message to the far side unchanged in
    /// intent. Every non-`CryptSetup` message takes this path.
    Forward(ControlMessage),
    /// Answer the message toward the side it came from (a resync reply the proxy
    /// generates itself, on behalf of the peer it is impersonating).
    ReplyToSender(ControlMessage),
    /// Absorb the message at the proxy boundary; nothing crosses. Used for nonce
    /// resyncs, which are domain-local and must not leak to the other side.
    Drop,
}

/// Errors from processing a control message. All are fail-closed: the relay logs
/// the error and tears the connection down rather than forwarding blindly.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// A `CryptSetup` field was present but not the required 16 bytes.
    #[error("CryptSetup field `{field}` is {len} bytes, expected {expected}")]
    BadLength {
        field: &'static str,
        len: usize,
        expected: usize,
    },

    /// The server sent a `CryptSetup` shape the proxy does not expect from a
    /// server (only a full key exchange or a lone `server_nonce` resync are).
    #[error("unexpected CryptSetup from server")]
    UnexpectedServerCryptSetup,

    /// The client sent a `CryptSetup` shape the proxy does not expect from a
    /// client (only an empty request or a lone `client_nonce` resync are).
    #[error("unexpected CryptSetup from client")]
    UnexpectedClientCryptSetup,

    /// A resync arrived before the initial key exchange established the domains.
    #[error("CryptSetup resync before the cipher was set up")]
    ResyncBeforeSetup,
}

/// The MITM control-plane state for one client<->server connection.
pub struct Session {
    secrets: ProxySecrets,
    channels: Option<CryptChannels>,
}

impl Session {
    pub fn new(secrets: ProxySecrets) -> Self {
        Self {
            secrets,
            channels: None,
        }
    }

    /// The established cipher domains, once the initial `CryptSetup` has arrived.
    pub fn channels(&self) -> Option<&CryptChannels> {
        self.channels.as_ref()
    }

    /// Mutable access to the cipher domains, so the UDP plane can decrypt on one
    /// side and re-encrypt on the other (both advance their IVs).
    pub fn channels_mut(&mut self) -> Option<&mut CryptChannels> {
        self.channels.as_mut()
    }

    /// Process a message travelling server -> client.
    pub fn process_from_server(&mut self, message: ControlMessage) -> Result<Action, SessionError> {
        match message {
            ControlMessage::CryptSetup(setup) => self.server_crypt_setup(setup),
            other => Ok(Action::Forward(other)),
        }
    }

    /// Process a message travelling client -> server.
    pub fn process_from_client(&mut self, message: ControlMessage) -> Result<Action, SessionError> {
        match message {
            ControlMessage::CryptSetup(setup) => self.client_crypt_setup(setup),
            other => Ok(Action::Forward(other)),
        }
    }

    /// Handle a `CryptSetup` sent by the server.
    ///
    /// Two valid shapes (see the module REF notes):
    /// - full (`key` + both nonces): the initial key exchange, or a server re-key.
    /// - `server_nonce` alone: the server resyncs its server->client encrypt IV.
    fn server_crypt_setup(&mut self, setup: tcp::CryptSetup) -> Result<Action, SessionError> {
        match (&setup.key, &setup.server_nonce, &setup.client_nonce) {
            (Some(key), Some(server_nonce), Some(client_nonce)) => {
                let key = key_array(key)?;
                let server_nonce = nonce_array("server_nonce", server_nonce)?;
                let client_nonce = nonce_array("client_nonce", client_nonce)?;

                // Proxy as client toward the server: it encrypts proxy->server
                // with the server's client_nonce (what the server will decrypt
                // with) and decrypts server->proxy with the server's server_nonce
                // (what the server encrypts with).
                let to_server = CryptState::new(&key, &client_nonce, &server_nonce);

                match self.channels.as_mut() {
                    // Mid-session re-key: only the server-facing domain changes;
                    // the client keeps the proxy's key. Absorb it (Drop).
                    Some(channels) => {
                        channels.to_server = to_server;
                        Ok(Action::Drop)
                    }
                    // Initial exchange: also build the client-facing domain from
                    // the proxy's own secrets and hand the client the proxy key.
                    None => {
                        let to_client = CryptState::new(
                            &self.secrets.key,
                            &self.secrets.encrypt_iv,
                            &self.secrets.decrypt_iv,
                        );
                        self.channels = Some(CryptChannels {
                            to_server,
                            to_client,
                        });
                        Ok(Action::Forward(ControlMessage::CryptSetup(
                            self.rewritten_setup_for_client(),
                        )))
                    }
                }
            }
            // Server resyncs its encrypt IV: apply to the server-facing decrypt
            // side and absorb. The client's decrypt is against the proxy's own
            // (unchanged) encrypt IV, so nothing crosses.
            (None, Some(server_nonce), None) => {
                let server_nonce = nonce_array("server_nonce", server_nonce)?;
                let channels = self
                    .channels
                    .as_mut()
                    .ok_or(SessionError::ResyncBeforeSetup)?;
                channels.to_server.set_decrypt_iv(&server_nonce);
                Ok(Action::Drop)
            }
            _ => Err(SessionError::UnexpectedServerCryptSetup),
        }
    }

    /// Handle a `CryptSetup` sent by the client.
    ///
    /// Two valid shapes (see the module REF notes). A client never sends `key`:
    /// - `client_nonce` alone: the client resyncs its client->server encrypt IV.
    /// - empty: the client requests a resync; the proxy answers as the server.
    fn client_crypt_setup(&mut self, setup: tcp::CryptSetup) -> Result<Action, SessionError> {
        // A client that sends a key is anomalous; the real server ignores it, but
        // we refuse loudly rather than second-guess (L4).
        if setup.key.is_some() || setup.server_nonce.is_some() {
            return Err(SessionError::UnexpectedClientCryptSetup);
        }
        match &setup.client_nonce {
            // Client resyncs its encrypt IV: apply to the client-facing decrypt
            // side and absorb (mirror of the server's setDecryptIV path).
            Some(client_nonce) => {
                let client_nonce = nonce_array("client_nonce", client_nonce)?;
                let channels = self
                    .channels
                    .as_mut()
                    .ok_or(SessionError::ResyncBeforeSetup)?;
                channels.to_client.set_decrypt_iv(&client_nonce);
                Ok(Action::Drop)
            }
            // Empty request: answer toward the client with the proxy's client-
            // facing encrypt IV, exactly as the server would answer with its own.
            None => {
                let channels = self
                    .channels
                    .as_ref()
                    .ok_or(SessionError::ResyncBeforeSetup)?;
                let reply = tcp::CryptSetup {
                    key: None,
                    client_nonce: None,
                    server_nonce: Some(channels.to_client.encrypt_iv().to_vec()),
                };
                Ok(Action::ReplyToSender(ControlMessage::CryptSetup(reply)))
            }
        }
    }

    /// The `CryptSetup` the proxy hands the client: its own key and nonces, so
    /// the client's `setKey(key, client_nonce, server_nonce)` mirrors the proxy's
    /// client-facing state (encrypt IV = server_nonce, decrypt IV = client_nonce).
    fn rewritten_setup_for_client(&self) -> tcp::CryptSetup {
        tcp::CryptSetup {
            key: Some(self.secrets.key.to_vec()),
            server_nonce: Some(self.secrets.encrypt_iv.to_vec()),
            client_nonce: Some(self.secrets.decrypt_iv.to_vec()),
        }
    }
}

/// Convert a wire key field to a fixed 16-byte array, rejecting any other length.
fn key_array(bytes: &[u8]) -> Result<[u8; KEY_SIZE], SessionError> {
    bytes.try_into().map_err(|_| SessionError::BadLength {
        field: "key",
        len: bytes.len(),
        expected: KEY_SIZE,
    })
}

/// Convert a wire nonce field to a fixed 16-byte array, rejecting any other length.
fn nonce_array(field: &'static str, bytes: &[u8]) -> Result<[u8; BLOCK_SIZE], SessionError> {
    bytes.try_into().map_err(|_| SessionError::BadLength {
        field,
        len: bytes.len(),
        expected: BLOCK_SIZE,
    })
}
