//! Configuration management with SIGHUP reload support (P2-12)

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Main configuration structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub audit: AuditConfig,
    pub approval: ApprovalConfig,
    pub metrics: MetricsConfig,
    pub agents: Vec<AgentConfig>,
    pub rules: Vec<RuleConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub bind: String,
    pub cert: String,
    pub key: String,
    pub client_ca: String,
    /// Path to CRL file for certificate revocation checking (P1-9)
    pub crl_file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditConfig {
    pub log_path: String,
    /// Rotation strategy: "daily", "hourly", or "never" (P2-14)
    #[serde(default = "default_rotation")]
    pub rotation: String,
    /// Number of days to retain audit logs
    #[serde(default = "default_retention")]
    pub retention_days: u32,
}

fn default_rotation() -> String {
    "daily".to_string()
}

fn default_retention() -> u32 {
    90
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalConfig {
    pub enabled: bool,
    pub ttl_seconds: u64,
    /// Token entropy in bits (80 = ~16 chars)
    #[serde(default = "default_token_entropy")]
    pub token_entropy_bits: u32,
    pub signal_webhook: Option<String>,
    /// Signal numbers allowed to approve
    pub allowed_approvers: Vec<String>,
    /// Secret key for HMAC token generation
    pub token_secret: Option<String>,
}

fn default_token_entropy() -> u32 {
    80
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub bind: String,
}

/// Agent identity configuration (P3-17)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Pattern to match certificate CN (e.g., "librarian*" or "claude-code-*")
    pub cn_pattern: String,
    /// Agent type: librarian, lucien, zeroclaw, acp_harness, etc.
    pub agent_type: String,
    /// Unix user to run operations as
    pub unix_user: String,
    /// Autonomy level
    #[serde(default)]
    pub autonomy: AutonomyLevel,
    /// Operations allowed without approval
    #[serde(default)]
    pub allowed_operations: Vec<String>,
    /// Operations that always require approval
    #[serde(default)]
    pub requires_approval_for: Vec<String>,
    /// Pattern-based rules
    #[serde(default)]
    pub pattern_rules: Vec<PatternRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternRule {
    pub operation: String,
    pub pattern: String,
}

/// Autonomy levels for agents
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyLevel {
    /// No destructive operations allowed
    ReadOnly,
    /// Requires approval for sensitive operations (default)
    Supervised,
    /// Full autonomy (careful!)
    Full,
}

impl Default for AutonomyLevel {
    fn default() -> Self {
        AutonomyLevel::Supervised
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleConfig {
    pub operation: String,
    pub approval_required: bool,
    pub pattern: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                bind: "127.0.0.1:18443".to_string(),
                cert: "/etc/clash/certs/server.crt".to_string(),
                key: "/etc/clash/certs/server.key".to_string(),
                client_ca: "/etc/clash/certs/ca.crt".to_string(),
                crl_file: None,
            },
            audit: AuditConfig {
                log_path: "/var/log/clash/audit.jsonl".to_string(),
                rotation: "daily".to_string(),
                retention_days: 90,
            },
            approval: ApprovalConfig {
                enabled: true,
                ttl_seconds: 300,
                token_entropy_bits: 80,
                signal_webhook: None,
                allowed_approvers: vec![],
                token_secret: None,
            },
            metrics: MetricsConfig {
                enabled: true,
                bind: "127.0.0.1:19090".to_string(),
            },
            agents: vec![
                AgentConfig {
                    cn_pattern: "librarian*".to_string(),
                    agent_type: "librarian".to_string(),
                    unix_user: "librarian".to_string(),
                    autonomy: AutonomyLevel::Supervised,
                    allowed_operations: vec!["zfs-list".to_string(), "zfs-snapshot".to_string()],
                    requires_approval_for: vec!["zfs-destroy".to_string()],
                    pattern_rules: vec![],
                },
                AgentConfig {
                    cn_pattern: "claude-code*".to_string(),
                    agent_type: "acp_harness".to_string(),
                    unix_user: "clash-agent".to_string(),
                    autonomy: AutonomyLevel::Supervised,
                    allowed_operations: vec!["zfs-list".to_string()],
                    requires_approval_for: vec!["zfs-snapshot".to_string(), "zfs-destroy".to_string()],
                    pattern_rules: vec![],
                },
            ],
            rules: vec![
                RuleConfig {
                    operation: "zfs-destroy".to_string(),
                    approval_required: true,
                    pattern: None,
                },
                RuleConfig {
                    operation: "zfs-snapshot".to_string(),
                    approval_required: false,
                    pattern: None,
                },
                RuleConfig {
                    operation: "zfs-list".to_string(),
                    approval_required: false,
                    pattern: None,
                },
            ],
        }
    }
}

impl Config {
    /// Load configuration from file, or create default if not exists
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        if path.exists() {
            let contents = std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read config file: {:?}", path))?;

            let config: Config = toml::from_str(&contents)
                .with_context(|| format!("Failed to parse config file: {:?}", path))?;

            info!("Loaded config from {:?}", path);
            Ok(config)
        } else {
            info!("Config file not found at {:?}, using defaults", path);
            Ok(Config::default())
        }
    }

    /// Check if an operation requires approval (P0-4)
    pub fn requires_approval(&self, operation: &str, target: &str) -> bool {
        self.rules
            .iter()
            .find(|r| r.operation == operation)
            .map(|r| {
                if !r.approval_required {
                    return false;
                }
                // Check pattern if present
                if let Some(ref pattern) = r.pattern {
                    let regex = regex::Regex::new(pattern).ok();
                    if let Some(re) = regex {
                        return re.is_match(target);
                    }
                }
                true
            })
            .unwrap_or(false)
    }

    /// Find agent config by CN
    pub fn find_agent(&self, cn: &str) -> Option<&AgentConfig> {
        self.agents.iter().find(|a| {
            if a.cn_pattern.ends_with('*') {
                let prefix = &a.cn_pattern[..a.cn_pattern.len()-1];
                cn.starts_with(prefix)
            } else {
                a.cn_pattern == cn
            }
        })
    }
}

/// Reloadable configuration wrapper (P2-12)
pub struct ReloadableConfig {
    inner: Arc<RwLock<Config>>,
    path: String,
}

impl ReloadableConfig {
    pub fn new(config: Config, path: String) -> Self {
        Self {
            inner: Arc::new(RwLock::new(config)),
            path,
        }
    }

    pub async fn get(&self) -> Config {
        self.inner.read().await.clone()
    }

    /// Reload configuration from disk (P2-12: SIGHUP handler calls this)
    pub async fn reload(&self) -> Result<()> {
        let new_config = Config::load(&self.path)?;
        let mut config = self.inner.write().await;
        *config = new_config;
        info!("Configuration reloaded from {}", self.path);
        Ok(())
    }

    pub fn subscribe_reload(&self) -> ConfigReloadHandle {
        ConfigReloadHandle {
            inner: self.inner.clone(),
        }
    }
}

/// Handle for accessing config after subscribing to reloads
#[derive(Clone)]
pub struct ConfigReloadHandle {
    inner: Arc<RwLock<Config>>,
}

impl ConfigReloadHandle {
    pub async fn get(&self) -> Config {
        self.inner.read().await.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_requires_approval() {
        let config = Config::default();
        
        // zfs-destroy requires approval
        assert!(config.requires_approval("zfs-destroy", "tank/media"));
        
        // zfs-list does not
        assert!(!config.requires_approval("zfs-list", "tank/media"));
    }

    #[test]
    fn test_find_agent() {
        let config = Config::default();
        
        // Should find librarian config
        let agent = config.find_agent("librarian");
        assert!(agent.is_some());
        assert_eq!(agent.unwrap().agent_type, "librarian");
        
        // Should find librarian-main via wildcard
        let agent = config.find_agent("librarian-main");
        assert!(agent.is_some());
        
        // Should not find unknown agent
        let agent = config.find_agent("unknown-agent");
        assert!(agent.is_none());
    }

    #[test]
    fn test_autonomy_level_deserialize() {
        let toml_str = r#"
            level = "supervised"
        "#;
        let level: AutonomyLevel = toml::from_str(toml_str).unwrap();
        assert_eq!(level, AutonomyLevel::Supervised);
    }
}
