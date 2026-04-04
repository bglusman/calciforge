//! TLS/mTLS configuration with client identity extraction

use anyhow::{Context, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use crate::auth::{build_identity, cert_fingerprint, is_cert_revoked, ClientIdentity};

/// Create mTLS server configuration
pub fn create_mtls_config<P: AsRef<Path>>(
    cert_path: P,
    key_path: P,
    client_ca_path: P,
    crl_path: Option<P>,
) -> Result<Arc<ServerConfig>> {
    let cert_path = cert_path.as_ref();
    let key_path = key_path.as_ref();
    let client_ca_path = client_ca_path.as_ref();

    // Load server certificate
    let cert_chain = load_certs(cert_path)
        .with_context(|| format!("Failed to load server cert: {:?}", cert_path))?;

    // Load server private key
    let key = load_private_key(key_path)
        .with_context(|| format!("Failed to load server key: {:?}", key_path))?;

    // Load client CA
    let client_ca = load_certs(client_ca_path)
        .with_context(|| format!("Failed to load client CA: {:?}", client_ca_path))?;

    // Load CRL if provided
    let crl_data = if let Some(crl_path) = crl_path {
        Some(fs::read(crl_path).with_context(|| "Failed to read CRL file")?)
    } else {
        None
    };

    // Create root certificate store
    let mut root_store = rustls::RootCertStore::empty();
    for cert in &client_ca {
        root_store
            .add(cert.clone())
            .with_context(|| "Failed to add client CA to root store")?;
    }

    // Create client certificate verifier
    let client_verifier = rustls::server::WebPkiClientVerifier::builder(
        Arc::new(root_store)
    )
    .build()
    .with_context(|| "Failed to create client certificate verifier")?;

    // Build server config with mTLS
    let rustls_config = ServerConfig::builder()
        .with_client_cert_verifier(client_verifier)
        .with_single_cert(cert_chain, key)
        .with_context(|| "Failed to create TLS config")?;

    info!("mTLS configuration created successfully");

    Ok(Arc::new(rustls_config))
}

/// Load certificates from PEM file
fn load_certs<P: AsRef<Path>>(path: P) -> Result<Vec<CertificateDer<'static>>> {
    let path = path.as_ref();
    let content = fs::read(path)
        .with_context(|| format!("Failed to read cert file: {:?}", path))?;

    let mut certs = Vec::new();
    let mut reader = std::io::BufReader::new(&content[..]);

    for item in rustls_pemfile::read_all(&mut reader) {
        match item? {
            rustls_pemfile::Item::X509Certificate(cert) => {
                certs.push(cert.into());
            }
            _ => {
                // Skip non-certificate items
            }
        }
    }

    if certs.is_empty() {
        anyhow::bail!("No certificates found in {:?}", path);
    }

    Ok(certs)
}

/// Load private key from PEM file
fn load_private_key<P: AsRef<Path>>(path: P) -> Result<PrivateKeyDer<'static>> {
    let path = path.as_ref();
    let content = fs::read(path)
        .with_context(|| format!("Failed to read key file: {:?}", path))?;

    let mut reader = std::io::BufReader::new(&content[..]);

    for item in rustls_pemfile::read_all(&mut reader) {
        match item? {
            rustls_pemfile::Item::Pkcs8Key(key) => {
                return Ok(key.into());
            }
            rustls_pemfile::Item::Sec1Key(key) => {
                // EC key in SEC1 format
                return Ok(key.into());
            }
            _ => {}
        }
    }

    anyhow::bail!("No valid private key found in {:?}", path)
}

/// Custom TLS acceptor that extracts client identity
pub struct IdentityExtractingAcceptor {
    inner: tokio_rustls::TlsAcceptor,
    crl_data: Option<Vec<u8>>,
}

impl IdentityExtractingAcceptor {
    pub fn new(config: Arc<ServerConfig>, crl_data: Option<Vec<u8>>) -> Self {
        Self {
            inner: tokio_rustls::TlsAcceptor::from(config),
            crl_data,
        }
    }

    /// Accept a TLS connection and extract client identity
    pub async fn accept(
        &self,
        stream: tokio::net::TcpStream,
    ) -> Result<(ClientIdentity, tokio_rustls::server::TlsStream<tokio::net::TcpStream>)> {
        let tls_stream = self.inner.accept(stream).await
            .with_context(|| "TLS handshake failed")?;

        // Get peer certificates
        let peer_certs = tls_stream.get_ref().1.peer_certificates();

        if let Some(certs) = peer_certs {
            if let Some(cert) = certs.first() {
                // Check revocation
                if is_cert_revoked(cert, self.crl_data.as_deref())? {
                    anyhow::bail!("Client certificate has been revoked");
                }

                // Build client identity
                let identity = build_identity(cert)
                    .with_context(|| "Failed to build client identity from certificate")?;

                debug!(
                    cn = %identity.cn,
                    fingerprint = %identity.fingerprint,
                    "Extracted client identity"
                );

                return Ok((identity, tls_stream));
            }
        }

        anyhow::bail!("No client certificate presented (mTLS required)")
    }
}

/// Test helper to generate self-signed certs for testing (updated for rcgen 0.13)
#[cfg(test)]
pub mod test_certs {
    use rcgen::{CertifiedKey, CertificateParams, KeyPair, IsCa, BasicConstraints, KeyUsagePurpose};

    /// Generate a self-signed CA certificate. Returns (CertifiedKey) which holds
    /// the cert + key pair together.
    pub fn generate_test_ca() -> (CertifiedKey, Vec<u8>) {
        let key_pair = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec!["ZeroClawed CA".to_string()]).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        let cert = params.self_signed(&key_pair).unwrap();
        let pem = cert.pem().into_bytes();
        (CertifiedKey { cert, key_pair }, pem)
    }

    /// Generate a client cert signed by the given CA.
    pub fn generate_test_client_cert(ca: &CertifiedKey, cn: &str) -> (Vec<u8>, Vec<u8>) {
        let key_pair = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec![cn.to_string()]).unwrap();
        params.distinguished_name.push(rcgen::DnType::CommonName, cn);
        let cert = params.signed_by(&key_pair, &ca.cert, &ca.key_pair).unwrap();
        let cert_pem = cert.pem().into_bytes();
        let key_pem = key_pair.serialize_pem().into_bytes();
        (cert_pem, key_pem)
    }
}
