//! Server-side TLS setup.
//!
//! The control channel is TLS, like Murmur. We pin TLS 1.2 for the same reason
//! the P2 proxy does: the macOS Qt/OpenSSL Mumble client segfaults in its
//! post-handshake introspection when handed a TLS 1.3 session, and Murmur itself
//! negotiates 1.2 by default. See `docs/STATUS.md` (macOS client trap).

use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// Install the ring crypto provider as the process default. Idempotent.
pub fn install_crypto_provider() {
    let _ignored = rustls::crypto::ring::default_provider().install_default();
}

/// A TLS certificate and its private key, in DER.
pub struct Identity {
    pub cert: CertificateDer<'static>,
    pub key: PrivateKeyDer<'static>,
}

impl Identity {
    /// Generate a fresh self-signed certificate for the given DNS names. Enough
    /// for local development and the in-process integration tests; a real
    /// deployment supplies a persistent certificate so the client's per-server
    /// certificate hash (spec §21.2) stays stable.
    pub fn self_signed(names: Vec<String>) -> Result<Self> {
        let certified = rcgen::generate_simple_self_signed(names)
            .context("generating self-signed certificate")?;
        let cert = certified.cert.der().clone();
        let key =
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der()));
        Ok(Self { cert, key })
    }
}

/// Build the server TLS config from an identity, pinned to TLS 1.2.
pub fn server_config(identity: Identity) -> Result<Arc<ServerConfig>> {
    let config = ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS12])
        .with_no_client_auth()
        .with_single_cert(vec![identity.cert], identity.key)
        .context("building server TLS config")?;
    Ok(Arc::new(config))
}
