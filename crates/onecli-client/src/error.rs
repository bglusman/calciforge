//! Error types for OneCLI client

use thiserror::Error;

pub type Result<T> = std::result::Result<T, OneCliError>;

#[derive(Error, Debug)]
pub enum OneCliError {
    #[error("OneCLI not reachable at {url}: {source}")]
    Unreachable {
        url: String,
        source: reqwest::Error,
    },

    #[error("Policy denied: {0}")]
    PolicyDenied(String),

    #[error("Rate limited: retry after {retry_after}s")]
    RateLimited { retry_after: u64 },

    #[error("Credential not found: {0}")]
    CredentialNotFound(String),

    #[error("Approval required: {0}")]
    ApprovalRequired(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl OneCliError {
    /// Check if the error is retryable
    pub fn is_retryable(&self) -> bool {
        matches!(self, 
            OneCliError::Unreachable { .. } |
            OneCliError::RateLimited { .. } |
            OneCliError::Http(_) 
        )
    }

    /// Get retry delay if applicable
    pub fn retry_delay(&self) -> Option<std::time::Duration> {
        match self {
            OneCliError::RateLimited { retry_after } => {
                Some(std::time::Duration::from_secs(*retry_after))
            }
            _ => None,
        }
    }
}
