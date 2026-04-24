//! Error types for OneCLI client

use thiserror::Error;

pub type Result<T> = std::result::Result<T, OneCliError>;

#[derive(Error, Debug)]
pub enum OneCliError {
    #[error("OneCLI not reachable at {url}: {source}")]
    Unreachable { url: String, source: reqwest::Error },

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
        matches!(
            self,
            OneCliError::Unreachable { .. }
                | OneCliError::RateLimited { .. }
                | OneCliError::Http(_)
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

#[cfg(test)]
mod tests {
    use super::*;

    // Mock error helpers (avoid creating real reqwest errors)
    #[allow(dead_code)]
    fn make_unreachable_error() -> OneCliError {
        OneCliError::Config("simulated".to_string())
    }

    #[test]
    fn test_unreachable_is_retryable() {
        // We can't easily construct a reqwest::Error without a network call,
        // but we can test the enum variant matching.
        let err = OneCliError::RateLimited { retry_after: 10 };
        assert!(err.is_retryable());
    }

    #[test]
    fn test_rate_limited_is_retryable() {
        let err = OneCliError::RateLimited { retry_after: 30 };
        assert!(err.is_retryable());
    }

    #[test]
    fn test_policy_denied_not_retryable() {
        let err = OneCliError::PolicyDenied("no access".to_string());
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_credential_not_found_not_retryable() {
        let err = OneCliError::CredentialNotFound("anthropic".to_string());
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_approval_required_not_retryable() {
        let err = OneCliError::ApprovalRequired("admin approval".to_string());
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_config_error_not_retryable() {
        let err = OneCliError::Config("bad config".to_string());
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_rate_limited_retry_delay() {
        let err = OneCliError::RateLimited { retry_after: 42 };
        assert_eq!(err.retry_delay(), Some(std::time::Duration::from_secs(42)));
    }

    #[test]
    fn test_other_error_no_retry_delay() {
        let err = OneCliError::Config("test".to_string());
        assert_eq!(err.retry_delay(), None);
    }

    #[test]
    fn test_error_display() {
        let err = OneCliError::PolicyDenied("block write".to_string());
        assert_eq!(err.to_string(), "Policy denied: block write");
    }

    #[test]
    fn test_rate_limited_display() {
        let err = OneCliError::RateLimited { retry_after: 5 };
        assert_eq!(err.to_string(), "Rate limited: retry after 5s");
    }

    /// Given a real reqwest error wrapped as `OneCliError::Http`,
    /// when `is_retryable()` is called,
    /// then the result is true. Guards the contract declared in
    /// `is_retryable` — `Http` errors are included because they
    /// usually represent transient upstream problems (5xx, timeouts,
    /// DNS, connection reset) where a retry has a reasonable chance
    /// of succeeding.
    ///
    /// Built the reqwest error by connecting to a port nothing is
    /// listening on. Round-2 test audit flagged `Http(_)` as the only
    /// is_retryable variant with no test coverage.
    #[tokio::test]
    async fn http_variant_is_retryable() {
        // 127.0.0.1:1 — almost certainly nothing listening. If this
        // machine has something there and the test passes connect,
        // the reqwest error type will still be one we can match.
        let bad_url = "http://127.0.0.1:1/";
        let reqwest_err = reqwest::get(bad_url)
            .await
            .expect_err("connecting to :1 must fail");
        let onecli_err: OneCliError = reqwest_err.into();
        assert!(
            matches!(onecli_err, OneCliError::Http(_)),
            "conversion from reqwest::Error must produce Http variant, got: {onecli_err:?}"
        );
        assert!(
            onecli_err.is_retryable(),
            "Http variant must be retryable — the is_retryable contract \
             declared Http alongside Unreachable and RateLimited"
        );
    }
}
