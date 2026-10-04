//! TLS helpers: fingerprint-pinned client + server acceptor.
//! Security model: the client pins the full certificate SHA-256 fingerprint and
//! verifies the TLS handshake signature with rustls. Pinning replaces name/CA validation.

use anyhow::{Context, Result};
use std::sync::Arc;

/// Explicitly install the rustls crypto provider (ring). Every binary that uses TLS
/// must call this once at startup (otherwise "Could not determine CryptoProvider").
pub fn init_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
use tokio_rustls::rustls;

#[derive(Debug)]
struct FpVerifier {
    expected: Option<String>,
}

impl rustls::client::danger::ServerCertVerifier for FpVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        match &self.expected {
            Some(fp) => {
                let norm = |s: &str| s.replace(':', "").to_lowercase();
                let got = super::fingerprint_full(end_entity.as_ref());
                if norm(&got) == norm(fp) {
                    Ok(rustls::client::danger::ServerCertVerified::assertion())
                } else {
                    tracing::warn!("CERTIFICATE MISMATCH! (possible MITM!)");
                    Err(rustls::Error::General("certificate fingerprint mismatch".into()))
                }
            }
            None => Err(rustls::Error::General(
                "no server fingerprint: RF_RV_FP is required".into(),
            )),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        let provider = rustls::crypto::ring::default_provider();
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        let provider = rustls::crypto::ring::default_provider();
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Connect to the server over TLS.
/// If fp is given, the certificate fingerprint is pinned (for self-signed certificates).
/// Without fp, normal validation against the bundled root certificates is used
/// (with a domain + Let's Encrypt there is no need to deal with fingerprints).
pub async fn tls_connect(
    addr: &str,
    server_name: &str,
    fp: Option<String>,
) -> Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>> {
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .with_context(|| format!("TCP connection timed out: {addr}"))?
    .with_context(|| format!("TCP connection failed: {addr}"))?;
    crate::tune_tcp(&stream);
    let config = match fp {
        Some(fp) => rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(FpVerifier { expected: Some(fp) }))
            .with_no_client_auth(),
        None => {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth()
        }
    };
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let name = rustls::pki_types::ServerName::try_from(server_name.to_string())
        .map_err(|_| anyhow::anyhow!("invalid server name"))?;
    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        connector.connect(name, stream),
    )
    .await
    .context("TLS handshake timed out")?
    .context("TLS handshake failed")
}

/// Reads the server's current certificate fingerprint WITHOUT verifying it.
/// ONLY for TOFU (show it to the user on first connection and ask for confirmation).
/// No sensitive data is sent over this connection; after confirmation a pinned connection is used.
pub async fn fetch_server_fingerprint(addr: &str, server_name: &str) -> Result<String> {
    #[derive(Debug)]
    struct AcceptAny;
    impl rustls::client::danger::ServerCertVerifier for AcceptAny {
        fn verify_server_cert(
            &self,
            _end_entity: &rustls::pki_types::CertificateDer<'_>,
            _intermediates: &[rustls::pki_types::CertificateDer<'_>],
            _server_name: &rustls::pki_types::ServerName<'_>,
            _ocsp: &[u8],
            _now: rustls::pki_types::UnixTime,
        ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &rustls::pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &rustls::pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }

    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .with_context(|| format!("TCP connection timed out: {addr}"))?
    .with_context(|| format!("TCP connection failed: {addr}"))?;
    crate::tune_tcp(&stream);
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny))
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let name = rustls::pki_types::ServerName::try_from(server_name.to_string())
        .map_err(|_| anyhow::anyhow!("invalid server name"))?;
    let tls = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        connector.connect(name, stream),
    )
    .await
    .context("TLS handshake timed out")?
    .context("TLS handshake failed")?;
    let (_, conn) = tls.get_ref();
    let cert = conn
        .peer_certificates()
        .and_then(|certs| certs.first())
        .context("server did not send a certificate")?;
    Ok(super::fingerprint_full(cert.as_ref()))
}
/// Server-side TLS acceptor (from certificate files).
pub fn tls_acceptor(
    cert_path: &str,
    key_path: &str,
) -> Result<tokio_rustls::TlsAcceptor> {
    use rustls_pemfile::{certs, pkcs8_private_keys, rsa_private_keys};
    use std::io::BufReader;

    let cert_file = std::fs::File::open(cert_path)
        .with_context(|| format!("could not open certificate: {cert_path}"))?;
    let mut reader = BufReader::new(cert_file);
    let certs: Vec<_> = certs(&mut reader).collect::<Result<_, _>>()?;
    if certs.is_empty() {
        anyhow::bail!("no certificate found in certificate file");
    }
    let key_file =
        std::fs::File::open(key_path).with_context(|| format!("could not open key: {key_path}"))?;
    let mut reader = BufReader::new(key_file);
    let keys = pkcs8_private_keys(&mut reader).collect::<Result<Vec<_>, _>>()?;
    if keys.is_empty() {
        let key_file = std::fs::File::open(key_path)?;
        let mut reader = BufReader::new(key_file);
        let rsa_keys = rsa_private_keys(&mut reader).collect::<Result<Vec<_>, _>>()?;
        let first = rsa_keys.into_iter().next().context("private key not found")?;
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, rustls::pki_types::PrivateKeyDer::Pkcs1(first))?;
        return Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)));
    }
    let key = keys.into_iter().next().context("private key not found")?;
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, rustls::pki_types::PrivateKeyDer::Pkcs8(key))?;
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}
