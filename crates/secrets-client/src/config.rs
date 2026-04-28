//! Retry configuration for the secrets client.
//!
//! Only `RetryConfig` lives here now. The broader service config types were
//! tied to an older HTTP credential-proxy design and have been removed.
//! `RetryConfig` remains because calciforge/proxy/retry.rs still imports it.
//! If that import goes away, this module can collapse.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Configuration for retry behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    pub max_retries: u32,
    #[serde(with = "humantime_serde")]
    pub base_delay: Duration,
    #[serde(with = "humantime_serde")]
    pub max_delay: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(10),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TOML round-trip for the default config — proves serde wiring +
    /// humantime_serde adapters work for both fields. (Behavioral, not
    /// tautological — exercises the actual serde machinery instead of
    /// re-asserting hard-coded constants.)
    #[test]
    fn retry_config_toml_roundtrip_preserves_all_fields() {
        let config = RetryConfig {
            max_retries: 5,
            base_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(30),
        };
        let toml_str = toml::to_string(&config).unwrap();
        let parsed: RetryConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.max_retries, 5);
        assert_eq!(parsed.base_delay, Duration::from_millis(250));
        assert_eq!(parsed.max_delay, Duration::from_secs(30));
    }
}
