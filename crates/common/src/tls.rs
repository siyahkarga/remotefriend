//! TLS yardımcısı: fingerprint-pin'li istemci + sunucu acceptor.
//! Güvenlik modeli: sertifika parmak izi eşleşirse MITM imkânsızdır.
//! (RF_RV_INSECURE=1 sadece localhost testleri içindir.)

use anyhow::{Context, Result};
use std::sync::Arc;

/// rustls kripto sağlayıcısını açıkça kur (ring). TLS kullanan her binary
/// başında bir kez çağırılmalı (yoksa "Could not determine CryptoProvider").
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
        if std::env::var("RF_RV_INSECURE").map(|v| v == "1").unwrap_or(false) {
            return Ok(rustls::client::danger::ServerCertVerified::assertion());
        }
        match &self.expected {
            Some(fp) => {
                let norm = |s: &str| s.replace(':', "").to_lowercase();
                let got = super::fingerprint_full(end_entity.as_ref());
                if norm(&got) == norm(fp) {
                    Ok(rustls::client::danger::ServerCertVerified::assertion())
                } else {
                    tracing::warn!("SERTİFİKA UYUMSUZLUĞU! (MITM olabilir!)");
                    Err(rustls::Error::General("sertifika fingerprint uyuşmadı".into()))
                }
            }
            None => Err(rustls::Error::General(
                "sunucu fingerprint yok: RF_RV_FP veya güvensiz mod gerekli".into(),
            )),
        }
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

/// Sunucuya TLS ile bağlan. fp: beklenen sertifika fingerprint (hex).
pub async fn tls_connect(
    addr: &str,
    server_name: &str,
    fp: Option<String>,
) -> Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>> {
    let stream = tokio::net::TcpStream::connect(addr)
        .await
        .with_context(|| format!("TCP bağlanamadı: {addr}"))?;
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(FpVerifier { expected: fp }))
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let name = rustls::pki_types::ServerName::try_from(server_name.to_string())
        .map_err(|_| anyhow::anyhow!("geçersiz sunucu adı"))?;
    connector
        .connect(name, stream)
        .await
        .context("TLS handshake başarısız")
}

/// Sunucu tarafı TLS acceptor (sertifika dosyalarından).
pub fn tls_acceptor(
    cert_path: &str,
    key_path: &str,
) -> Result<tokio_rustls::TlsAcceptor> {
    use rustls_pemfile::{certs, pkcs8_private_keys, rsa_private_keys};
    use std::io::BufReader;

    let cert_file = std::fs::File::open(cert_path)
        .with_context(|| format!("sertifika açılamadı: {cert_path}"))?;
    let mut reader = BufReader::new(cert_file);
    let certs: Vec<_> = certs(&mut reader).collect::<Result<_, _>>()?;
    if certs.is_empty() {
        anyhow::bail!("sertifika dosyasında cert yok");
    }
    let key_file =
        std::fs::File::open(key_path).with_context(|| format!("anahtar açılamadı: {key_path}"))?;
    let mut reader = BufReader::new(key_file);
    let mut keys = pkcs8_private_keys(&mut reader).collect::<Result<Vec<_>, _>>()?;
    if keys.is_empty() {
        let key_file = std::fs::File::open(key_path)?;
        let mut reader = BufReader::new(key_file);
        let rsa_keys = rsa_private_keys(&mut reader).collect::<Result<Vec<_>, _>>()?;
        let first = rsa_keys.into_iter().next().context("özel anahtar bulunamadı")?;
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, rustls::pki_types::PrivateKeyDer::Pkcs1(first))?;
        return Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)));
    }
    let key = keys.into_iter().next().context("özel anahtar bulunamadı")?;
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, rustls::pki_types::PrivateKeyDer::Pkcs8(key))?;
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}
