//! TLS setup for the MITM proxy.
//!
//! The proxy terminates the client's TLS with a freshly generated self-signed
//! certificate and reconnects to the real Murmur server as a client that accepts
//! any server certificate. Accepting any certificate is acceptable here and ONLY
//! here: this is a local research/oracle tool pointed at a local Murmur, not a
//! security boundary. It must never be reused in the runtime.

use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, ServerConfig, SignatureScheme};

/// Install the ring crypto provider as the process default. Idempotent: a second
/// call (or a provider already installed) is ignored on purpose.
pub fn install_crypto_provider() {
    let _ignored = rustls::crypto::ring::default_provider().install_default();
}

/// Build a server config with a fresh self-signed certificate for `localhost`.
pub fn server_config() -> Result<Arc<ServerConfig>> {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .context("generating self-signed certificate")?;

    let cert_der: CertificateDer<'static> = certified.cert.der().clone();
    let key_der = PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der());
    let key = PrivateKeyDer::Pkcs8(key_der);

    // Restrict the client-facing side to TLS 1.2. A real Murmur negotiates 1.2
    // by default, and the macOS Qt/OpenSSL Mumble build segfaults in its post-
    // handshake introspection (sessionCipher/ephemeralServerKey) when handed a
    // TLS 1.3 session, where rustls otherwise prefers 1.3. Matching Murmur's
    // version keeps the oracle transparent to the real client.
    let config = ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS12])
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key)
        .context("building server TLS config")?;

    Ok(Arc::new(config))
}

/// Build a client config that accepts any server certificate. See the module
/// comment for why this is safe in this tool and nowhere else.
pub fn client_config() -> Arc<ClientConfig> {
    let algorithms = rustls::crypto::ring::default_provider().signature_verification_algorithms;
    let verifier = Arc::new(AcceptAnyServerCert { algorithms });
    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Arc::new(config)
}

/// A verifier that trusts any presented server certificate but still validates
/// handshake signatures against the ring provider's algorithms.
#[derive(Debug)]
struct AcceptAnyServerCert {
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}
