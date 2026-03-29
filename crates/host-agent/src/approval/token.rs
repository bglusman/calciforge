//! Secure approval token generation and validation (P1-5, P1-6)
//!
//! Tokens are 16-character HMAC-based strings with high entropy (~80 bits).
//! For logging, only SHA-256 hashes are stored, never plaintext tokens.

use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::{Digest, Sha256};
use std::fmt::Write;

// HMAC-SHA256 for token generation
type HmacSha256 = Hmac<Sha256>;

/// Token alphabet (removed confusing characters: I, O, 0, 1, l)
const TOKEN_CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789abcdefghijkmnopqrstuvwxyz";
const TOKEN_LENGTH: usize = 16;

/// Generate a high-entropy approval token (P1-5)
/// 
/// Returns a 16-character token with ~80 bits of entropy.
/// Uses cryptographically secure random number generation.
pub fn generate_token() -> String {
    let mut rng = rand::thread_rng();
    
    (0..TOKEN_LENGTH)
        .map(|_| {
            let idx = rng.gen_range(0..TOKEN_CHARSET.len());
            TOKEN_CHARSET[idx] as char
        })
        .collect()
}

/// Generate a cryptographically secure token using HMAC (alternative method)
/// 
/// This creates tokens with HMAC-SHA256 for additional security.
/// Requires a secret key from configuration.
pub fn generate_hmac_token(secret_key: &[u8], context: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret_key)
        .expect("HMAC can take key of any size");
    
    // Include timestamp and random nonce for uniqueness
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let nonce: u64 = rand::thread_rng().gen();
    
    mac.update(&timestamp.to_be_bytes());
    mac.update(&nonce.to_be_bytes());
    mac.update(context.as_bytes());
    
    let result = mac.finalize();
    let bytes = result.into_bytes();
    
    // Encode to alphanumeric (same charset as simple tokens)
    bytes_to_token(&bytes[..TOKEN_LENGTH])
}

/// Convert bytes to token string using our charset
fn bytes_to_token(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| {
            let idx = (*b as usize) % TOKEN_CHARSET.len();
            TOKEN_CHARSET[idx] as char
        })
        .collect()
}

/// Hash a token for secure logging (P1-6)
/// 
/// Returns the SHA-256 hash of the token as a hex string.
/// Store this in logs, never the plaintext token.
pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

/// Verify a token against its hash
pub fn verify_token_hash(token: &str, expected_hash: &str) -> bool {
    let actual_hash = hash_token(token);
    actual_hash == expected_hash
}

/// Format a token for display (first 4 chars only, rest masked)
pub fn mask_token(token: &str) -> String {
    if token.len() <= 4 {
        "****".to_string()
    } else {
        format!("{}****", &token[..4])
    }
}

/// Token metadata for audit logging
#[derive(Debug, Clone)]
pub struct TokenAuditInfo {
    /// SHA-256 hash of the token (store this!)
    pub hash: String,
    /// Masked token for display (e.g., "X7K9****")
    pub masked: String,
    /// First 8 chars of hash for correlation
    pub hash_prefix: String,
}

impl From<&str> for TokenAuditInfo {
    fn from(token: &str) -> Self {
        let hash = hash_token(token);
        let hash_prefix = hash[..8].to_string();
        Self {
            hash,
            masked: mask_token(token),
            hash_prefix,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_token_length() {
        let token = generate_token();
        assert_eq!(token.len(), TOKEN_LENGTH);
    }

    #[test]
    fn test_token_charset() {
        let token = generate_token();
        for ch in token.chars() {
            assert!(
                TOKEN_CHARSET.contains(&(ch as u8)),
                "Invalid character '{}' in token", ch
            );
        }
    }

    #[test]
    fn test_token_entropy() {
        // Generate many tokens and check for collisions
        let mut tokens = HashSet::new();
        for _ in 0..10000 {
            tokens.insert(generate_token());
        }
        // With 16 chars from 54-char alphabet, collision probability is negligible
        assert_eq!(tokens.len(), 10000, "Token collision detected!");
    }

    #[test]
    fn test_token_hashing() {
        let token = "X7K9M2P4Q8R5N6V3";
        let hash = hash_token(token);
        
        // Hash should be 64 hex chars
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        
        // Same token = same hash
        let hash2 = hash_token(token);
        assert_eq!(hash, hash2);
        
        // Different token = different hash (with high probability)
        let different_token = "Y8L0N3Q5R6S7T8U9";
        let different_hash = hash_token(different_token);
        assert_ne!(hash, different_hash);
    }

    #[test]
    fn test_verify_token_hash() {
        let token = "test-token-12345";
        let hash = hash_token(token);
        
        assert!(verify_token_hash(token, &hash));
        assert!(!verify_token_hash("wrong-token", &hash));
    }

    #[test]
    fn test_mask_token() {
        assert_eq!(mask_token("X7K9M2P4Q8R5N6V3"), "X7K9****");
        assert_eq!(mask_token("short"), "****");
        assert_eq!(mask_token("ab"), "****");
    }

    #[test]
    fn test_hmac_token() {
        let secret = b"test-secret-key";
        let context = "zfs-destroy:tank/media@snap";
        
        let token1 = generate_hmac_token(secret, context);
        let token2 = generate_hmac_token(secret, context);
        
        // HMAC tokens should be different (due to nonce)
        assert_ne!(token1, token2);
        
        // But both should be valid length
        assert_eq!(token1.len(), TOKEN_LENGTH);
        assert_eq!(token2.len(), TOKEN_LENGTH);
    }

    #[test]
    fn test_token_audit_info() {
        let token = "X7K9M2P4Q8R5N6V3";
        let info = TokenAuditInfo::from(token);
        
        assert_eq!(info.masked, "X7K9****");
        assert_eq!(info.hash.len(), 64);
        assert_eq!(info.hash_prefix.len(), 8);
        assert!(info.hash.starts_with(&info.hash_prefix));
    }
}
