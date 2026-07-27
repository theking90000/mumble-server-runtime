//! Server-side TLS setup.
//!
//! The control channel is TLS, like Murmur. We pin TLS 1.2 for the same reason
//! the P2 proxy does: the macOS Qt/OpenSSL Mumble client segfaults in its
//! post-handshake introspection when handed a TLS 1.3 session, and Murmur itself
//! negotiates 1.2 by default. See `docs/STATUS.md` (macOS client trap).

use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime};
use rustls::server::ParsedCertificate;
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    DigitallySignedStruct, DistinguishedName, Error as TlsError, ServerConfig, SignatureScheme,
};
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;

/// Requests a client certificate for identity while accepting anonymous clients.
///
/// A self-signed certificate is an identity proof, not an authorization proof.
/// The TLS CertificateVerify signature still proves possession of its private
/// key. Authentication and principal binding remain outside Phase 6.
///
/// REF: references/mumble/src/murmur/Server.cpp:Server::sslError
/// REF: references/mumble/src/murmur/Server.cpp:Server::encrypted
#[derive(Debug)]
struct MumbleClientCertificateVerifier {
    algorithms: WebPkiSupportedAlgorithms,
}

impl ClientCertVerifier for MumbleClientCertificateVerifier {
    fn client_auth_mandatory(&self) -> bool {
        false
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, TlsError> {
        ParsedCertificate::try_from(end_entity).map(|_| ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(message, certificate, signature, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(message, certificate, signature, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

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
///
/// REF: references/mumble/src/mumble/ServerHandler.cpp:ServerHandler::ServerHandler
/// REF: references/mumble/src/murmur/Server.cpp:Server::encrypted
pub fn server_config(identity: Identity) -> Result<Arc<ServerConfig>> {
    let algorithms = rustls::crypto::ring::default_provider().signature_verification_algorithms;
    let verifier = Arc::new(MumbleClientCertificateVerifier { algorithms });
    let config = ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS12])
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![identity.cert], identity.key)
        .context("building server TLS config")?;
    Ok(Arc::new(config))
}

/// Return Murmur's lowercase SHA-1 digest of the immediate client certificate.
///
/// This value is only presented in `UserState.hash` so the official client can
/// retain local preferences. It does not grant permissions.
///
/// REF: references/vendored/Mumble.proto:UserState.hash
/// REF: references/mumble/src/murmur/Server.cpp:Server::encrypted
pub fn client_certificate_hash(stream: &TlsStream<TcpStream>) -> Option<String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let certificate = stream.get_ref().1.peer_certificates()?.first()?;
    let digest = ring::digest::digest(
        &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
        certificate.as_ref(),
    );
    let mut encoded = String::with_capacity(digest.as_ref().len().checked_mul(2)?);
    for byte in digest.as_ref() {
        let high = HEX.get(usize::from(byte >> 4)).copied()?;
        let low = HEX.get(usize::from(byte & 0x0f)).copied()?;
        encoded.push(char::from(high));
        encoded.push(char::from(low));
    }
    Some(encoded)
}
