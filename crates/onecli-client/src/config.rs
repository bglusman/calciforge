//! OneCLI Service Configuration

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Configuration for retry behavior
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OneCliConfig {
    pub url: String,
    pub agent_id: String,
    #[serde(with = "humantime_serde")]
    pub timeout: std::time::Duration,
}

impl Default for OneCliConfig {
    fn default() -> Self {
        Self {
            url: "http://localhost:8081".to_string(),
            agent_id: "default".to_string(),
            timeout: std::time::Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OneCliServiceConfig {
    pub bind: String,
    pub vault: VaultConfig,
    pub policy_file: Option<PathBuf>,
    pub providers: ProviderConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultConfig {
    pub backend: String,
    pub url: Option<String>,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub anthropic: Option<String>,
    pub openai: Option<String>,
    pub kimi: Option<String>,
    pub gemini: Option<String>,
}

impl OneCliServiceConfig {
    pub async fn from_env_or_file() -> anyhow::Result<Self> {
        if let Ok(config_path) = std::env::var("ONECLI_CONFIG") {
            let contents = tokio::fs::read_to_string(&config_path).await?;
            return Ok(toml::from_str(&contents)?);
        }

        Ok(Self {
            bind: std::env::var("ONECLI_BIND").unwrap_or_else(|_| "0.0.0.0:8081".to_string()),
            vault: VaultConfig {
                backend: std::env::var("ONECLI_VAULT_BACKEND")
                    .unwrap_or_else(|_| "env".to_string()),
                url: std::env::var("ONECLI_VAULT_URL").ok(),
                password: std::env::var("ONECLI_VAULT_PASSWORD").unwrap_or_else(|_| "".to_string()),
            },
            policy_file: std::env::var("ONECLI_POLICY_FILE").ok().map(PathBuf::from),
            providers: ProviderConfig {
                anthropic: std::env::var("ANTHROPIC_BASE_URL").ok(),
                openai: std::env::var("OPENAI_BASE_URL").ok(),
                kimi: std::env::var("KIMI_BASE_URL").ok(),
                gemini: std::env::var("GEMINI_BASE_URL").ok(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retry_config_defaults() {
        let config = RetryConfig::default();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.base_delay, Duration::from_millis(100));
        assert_eq!(config.max_delay, Duration::from_secs(10));
    }

    #[test]
    fn test_onecli_config_defaults() {
        let config = OneCliConfig::default();
        assert_eq!(config.url, "http://localhost:8081");
        assert_eq!(config.agent_id, "default");
        assert_eq!(config.timeout, Duration::from_secs(30));
    }

    #[test]
    fn test_onecli_config_toml_roundtrip() {
        let config = OneCliConfig {
            url: "http://onecli:9090".to_string(),
            agent_id: "test-agent".to_string(),
            timeout: Duration::from_secs(60),
        };
        let toml_str = toml::to_string(&config).unwrap();
        let parsed: OneCliConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.url, config.url);
        assert_eq!(parsed.agent_id, config.agent_id);
        assert_eq!(parsed.timeout, config.timeout);
    }

    #[test]
    fn test_retry_config_toml_roundtrip() {
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
