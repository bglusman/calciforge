//! Authentication and identity resolution from mTLS client certificates

use rustls::pki_types::CertificateDer;
use sha2::{Digest, Sha256};
use tracing::{debug, error, warn};
use x509_parser::prelude::*;

/// Client identity extracted from mTLS certificate
#[derive(Debug, Clone)]
pub struct ClientIdentity {
    /// Common Name from client certificate
    pub cn: String,
    /// Resolved Unix UID
    pub uid: u32,
    /// Unix username
    pub username: String,
    /// Certificate SHA-256 fingerprint (for revocation checking)
    pub fingerprint: String,
}

/// Errors during identity resolution
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("Certificate parsing failed: {0}")]
    ParseError(String),
    #[error("No Common Name found in certificate")]
    MissingCN,
    #[error("User resolution failed for CN '{cn}': {reason}")]
    UserResolutionFailed { cn: String, reason: String },
    #[error("Root mapping not allowed")]
    RootNotAllowed,
}

/// Extract CN from a client certificate
pub fn extract_cn(cert: &CertificateDer<'_>) -> Result<String, IdentityError> {
    match parse_x509_certificate(cert.as_ref()) {
        Ok((_, cert)) => {
            // Look for Common Name in the subject
            // CN OID is 2.5.4.3
            for attr in cert.subject.iter_attributes() {
                if attr.attr_type().to_id_string() == "2.5.4.3" {
                    if let Ok(cn) = attr.attr_value().as_str() {
                        return Ok(cn.to_string());
                    }
                }
            }
            Err(IdentityError::MissingCN)
        }
        Err(e) => Err(IdentityError::ParseError(e.to_string())),
    }
}

/// Calculate certificate fingerprint (SHA-256)
pub fn cert_fingerprint(cert: &CertificateDer<'_>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(cert.as_ref());
    hex::encode(hasher.finalize())
}

/// Resolve CN to Unix user using system calls (P1-8: real UID lookup)
pub fn resolve_unix_user(cn: &str) -> Result<(String, u32), IdentityError> {
    // Security: explicitly reject root mapping
    if cn == "root" {
        warn!("Rejecting root CN mapping attempt");
        return Err(IdentityError::RootNotAllowed);
    }

    // Use nix crate's User::from_name which wraps libc getpwnam
    match nix::unistd::User::from_name(cn) {
        Ok(Some(user)) => {
            debug!(cn = %cn, uid = %user.uid, "Resolved CN to Unix user");
            // Convert nix::unistd::Uid to u32
            let uid: u32 = user.uid.as_raw();
            Ok((cn.to_string(), uid))
        }
        Ok(None) => {
            warn!(cn = %cn, "No Unix user found for CN");
            Err(IdentityError::UserResolutionFailed {
                cn: cn.to_string(),
                reason: "User not found in /etc/passwd".to_string(),
            })
        }
        Err(e) => {
            error!(cn = %cn, error = %e, "Failed to resolve Unix user");
            Err(IdentityError::UserResolutionFailed {
                cn: cn.to_string(),
                reason: e.to_string(),
            })
        }
    }
}

/// Build ClientIdentity from certificate
pub fn build_identity(cert: &CertificateDer<'_>) -> Result<ClientIdentity, IdentityError> {
    let cn = extract_cn(cert)?;
    let fingerprint = cert_fingerprint(cert);
    let (username, uid) = resolve_unix_user(&cn)?;

    Ok(ClientIdentity {
        cn,
        uid,
        username,
        fingerprint,
    })
}

/// Check if certificate is revoked (P1-9: CRL checking)
pub fn is_cert_revoked(
    cert: &CertificateDer<'_>,
    crl_data: Option<&[u8]>,
) -> Result<bool, IdentityError> {
    let fingerprint = cert_fingerprint(cert);

    // If no CRL configured, cert is not revoked
    let crl_data = match crl_data {
        Some(data) => data,
        None => return Ok(false),
    };

    // Parse CRL and check for our cert's fingerprint
    // In production, use x509_parser to properly parse CRL
    // For now, check if fingerprint appears in CRL data
    let crl_str = String::from_utf8_lossy(crl_data);

    // Simple line-based check (real implementation would parse ASN.1)
    for line in crl_str.lines() {
        if line.trim() == fingerprint {
            warn!(fingerprint = %fingerprint, "Certificate found in revocation list");
            return Ok(true);
        }
    }

    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_root_mapping_rejected() {
        let result = resolve_unix_user("root");
        assert!(matches!(result, Err(IdentityError::RootNotAllowed)));
    }

    #[test]
    fn test_fingerprint_format() {
        // Test that fingerprint is 64 hex chars
        let cert_data = vec![1u8, 2, 3, 4, 5];
        let cert = CertificateDer::from(cert_data);
        let fp = cert_fingerprint(&cert);
        assert_eq!(fp.len(), 64);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
