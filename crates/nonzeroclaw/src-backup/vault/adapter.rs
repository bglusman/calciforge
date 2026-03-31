//! `VaultAdapter` trait — backend-agnostic secret storage interface.

use crate::vault::{Secret, SecretValue, SessionToken, VaultError};
use async_trait::async_trait;

/// Result type for vault operations.
pub type VaultResult<T> = Result<T, VaultError>;

/// Backend-agnostic interface for a secret vault.
///
/// Implementations include:
/// - [`crate::vault::BitwardenCliAdapter`] (feature = "bitwarden-cli") — via `bw` subprocess
/// - `NoopVaultAdapter` — always returns `VaultError::NotConfigured`
///
/// The trait is object-safe via `async_trait`, allowing `Box<dyn VaultAdapter>`.
#[async_trait]
pub trait VaultAdapter: Send + Sync {
    /// Retrieve a secret by its logical key name.
    ///
    /// The concrete meaning of `key` (e.g. a Bitwarden item ID, an env var name)
    /// is determined by the adapter implementation.
    async fn get_secret(&self, key: &str) -> VaultResult<Secret>;

    /// Store or update a secret.
    async fn store_secret(&self, key: &str, value: SecretValue) -> VaultResult<()>;

    /// Unlock the vault and return a short-lived session token.
    ///
    /// Callers should cache the token and only call `unlock` again when the
    /// token has expired (`SessionToken::is_valid()` returns false).
    async fn unlock(&self) -> VaultResult<SessionToken>;

    /// Return a human-readable name for this adapter (for logging/display).
    fn name(&self) -> &'static str;
}

// ── NoopVaultAdapter ─────────────────────────────────────────────────────────

/// A no-op vault adapter that always returns `VaultError::NotConfigured`.
///
/// Used when `vault.backend = "none"` is set in config.
#[derive(Debug, Default, Clone)]
pub struct NoopVaultAdapter;

#[async_trait]
impl VaultAdapter for NoopVaultAdapter {
    async fn get_secret(&self, _key: &str) -> VaultResult<Secret> {
        Err(VaultError::NotConfigured)
    }

    async fn store_secret(&self, _key: &str, _value: SecretValue) -> VaultResult<()> {
        Err(VaultError::NotConfigured)
    }

    async fn unlock(&self) -> VaultResult<SessionToken> {
        Err(VaultError::NotConfigured)
    }

    fn name(&self) -> &'static str {
        "noop"
    }
}
