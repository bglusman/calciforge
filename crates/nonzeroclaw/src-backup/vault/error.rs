//! Vault error types.

use thiserror::Error;

/// Errors produced by the vault subsystem.
#[derive(Debug, Error)]
pub enum VaultError {
    /// The vault backend is not configured (`backend = "none"`).
    #[error("vault is not configured (backend = \"none\")")]
    NotConfigured,

    /// The requested secret key is not known to the vault config.
    #[error("unknown secret key: {0}")]
    UnknownKey(String),

    /// The vault is locked and could not be unlocked.
    #[error("vault unlock failed: {0}")]
    UnlockFailed(String),

    /// A session token was required but none is cached.
    #[error("no active vault session token; re-unlock required")]
    NoSessionToken,

    /// A `bw` subprocess call failed.
    #[error("bw CLI error: {0}")]
    CliError(String),

    /// The `bw` binary was not found at the configured path.
    #[error("bw binary not found at path '{0}'")]
    BinaryNotFound(String),

    /// The approval request was denied by the operator.
    #[error("secret access denied by operator for key '{0}'")]
    Denied(String),

    /// The approval request timed out with no response.
    #[error("approval request timed out for key '{0}'")]
    TimedOut(String),

    /// An I/O error occurred while running the vault command.
    #[error("vault I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A generic/unexpected error.
    #[error("vault error: {0}")]
    Other(#[from] anyhow::Error),
}

impl VaultError {
    /// Construct from a string message (for `CliError`).
    pub fn cli(msg: impl Into<String>) -> Self {
        Self::CliError(msg.into())
    }
}
