//! Configuration for OneCLI client

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// OneCLI client configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OneCliConfig {
    /// OneCLI server URL
    pub url: String,

    /// Agent identifier for policy matching
    pub agent_id: String,

    /// Request timeout
    #[serde(with = "humantime_serde", default = "default_timeout")]
    pub timeout: Duration,

    /// Retry configuration
    #[serde(default)]
    pub retry: RetryConfig,

    /// Whether to fail open if OneCLI is unavailable
    #[serde(default = "default_fail_open")]
    pub fail_open: bool,
}

impl Default for OneCliConfig {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:18799".to_string(),
            agent_id: "zeroclawed-default".to_string(),
            timeout: default_timeout(),
            retry: RetryConfig::default(),
            fail_open: default_fail_open(),
        }
    }
}

impl OneCliConfig {
    /// Create config from environment variables
    pub fn from_env() -> Self {
        Self {
            url: std::env::var("ONECLI_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:18799".to_string()),
            agent_id: std::env::var("ONECLI_AGENT_ID")
                .unwrap_or_else(|_| "zeroclawed-default".to_string()),
            timeout: std::env::var("ONECLI_TIMEOUT")
                .ok()
                .and_then(|s| s.parse().ok())
                .map(Duration::from_secs)
                .unwrap_or_else(default_timeout),
            retry: RetryConfig::default(),
            fail_open: std::env::var("ONECLI_FAIL_OPEN")
                .ok()
                .map(|s| s == "true" || s == "1")
                .unwrap_or_else(default_fail_open),
        }
    }

    /// Validate the configuration
    pub fn validate(&self) -> crate::Result<()> {
        if self.url.is_empty() {
            return Err(crate::OneCliError::Config(
                "OneCLI URL cannot be empty".to_string()
            ));
        }
        if self.agent_id.is_empty() {
            return Err(crate::OneCliError::Config(
                "Agent ID cannot be empty".to_string()
            ));
        }
        Ok(())
    }
}

/// Retry configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    /// Maximum number of retries
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,

    /// Base delay between retries
    #[serde(with = "humantime_serde", default = "default_retry_delay")]
    pub base_delay: Duration,

    /// Maximum delay between retries
    #[serde(with = "humantime_serde", default = "default_max_retry_delay")]
    pub max_delay: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: default_max_retries(),
            base_delay: default_retry_delay(),
            max_delay: default_max_retry_delay(),
        }
    }
}

fn default_timeout() -> Duration {
    Duration::from_secs(30)
}

fn default_fail_open() -> bool {
    false // Fail closed by default for security
}

fn default_max_retries() -> u32 {
    3
}

fn default_retry_delay() -> Duration {
    Duration::from_millis(100)
}

fn default_max_retry_delay() -> Duration {
    Duration::from_secs(5)
}

// Helper module for humantime serialization
mod humantime_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let secs = duration.as_secs();
        let millis = duration.subsec_millis();
        if millis == 0 {
            serializer.serialize_str(&format!("{}s", secs))
        } else {
            serializer.serialize_str(&format!("{}.{:03}s", secs, millis))
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        humantime::parse_duration(&s).map_err(serde::de::Error::custom)
    }
}
