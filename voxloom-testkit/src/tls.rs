//! TLS client setup for the simulated client.
//!
//! The simulated client trusts any server certificate: it is a local test judge
//! pointed at a local server, never a security boundary. It still validates
//! handshake signatures against the ring provider.

use std::sync::Arc;

use rustls::ClientConfig;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

/// Install the ring crypto provider as the process default. Idempotent.
pub fn install_crypto_provider() {
    let _ignored = rustls::crypto::ring::default_provider().install_default();
}

/// A client config that trusts any server certificate (test judge only).
pub fn client_config() -> Arc<ClientConfig> {
    let algorithms = rustls::crypto::ring::default_provider().signature_verification_algorithms;
    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(TrustAnyServer { algorithms }))
        .with_no_client_auth();
    Arc::new(config)
}

#[derive(Debug)]
struct TrustAnyServer {
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for TrustAnyServer {
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
