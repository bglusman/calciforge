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
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
    pub agents: Vec<AgentConfig>,
    pub rules: Vec<RuleConfig>,
    /// Git adapter configuration
    #[serde(default)]
    pub git: Option<GitConfig>,
    /// Exec adapter configuration
    #[serde(default)]
    pub exec: Option<ExecConfig>,
}

/// Git adapter configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitConfig {
    /// List of allowed repository root paths.
    /// Empty = all absolute paths allowed (not recommended for production).
    #[serde(default)]
    pub allowed_repos: Vec<String>,
}

/// Exec/Ansible adapter configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecConfig {
    /// Must be true to enable this adapter (disabled by default).
    #[serde(default)]
    pub enabled: bool,
    /// Absolute paths of commands that may be executed.
    #[serde(default)]
    pub allowed_commands: Vec<String>,
    /// Directory where Ansible job specs are written (stub).
    #[serde(default)]
    pub ansible_job_queue: Option<String>,
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
    /// If set, only clients whose CN matches this pattern can approve operations
    /// marked `approval_admin_only = true` in rules. Default: any mTLS client can approve.
    #[serde(default)]
    pub admin_cn_pattern: Option<String>,
    /// Optional out-of-process hook for approver identity validation.
    /// Format: "command:/path/to/bin", "http://127.0.0.1:PORT/validate", or unset.
    /// Hook receives JSON on stdin: {"approver_cn":"...", "approval_id":"...", "operation":"...", "target":"..."}
    /// Expected JSON response: {"allowed": true|false, "reason": "optional"}
    /// When unset, identity validation is skipped (mTLS is the only gate).
    #[serde(default)]
    pub identity_plugin: Option<String>,
}

fn default_token_entropy() -> u32 {
    80
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// Enable per-CN rate limiting on destructive endpoints
    #[serde(default = "default_rate_limit_enabled")]
    pub enabled: bool,
    /// Maximum requests per window
    #[serde(default = "default_rate_limit_max")]
    pub max_requests: u32,
    /// Window size in seconds
    #[serde(default = "default_rate_limit_window")]
    pub window_seconds: u64,
    /// Endpoints to rate-limit (default: destroy, approve, pending)
    #[serde(default = "default_rate_limited_endpoints")]
    pub endpoints: Vec<String>,
}

fn default_rate_limit_enabled() -> bool { true }
fn default_rate_limit_max() -> u32 { 5 }
fn default_rate_limit_window() -> u64 { 60 }
fn default_rate_limited_endpoints() -> Vec<String> {
    vec![
        "/zfs/destroy".to_string(),
        "/approve".to_string(),
        "/pending".to_string(),
    ]
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
    /// Allow Full-autonomy agents to bypass operations marked `always_ask = true`.
    /// Default: false (safe). Must be explicitly set true per-agent by admin to allow bypass.
    /// Even when true, bypass only applies if the global rule does NOT mark `always_ask = true`.
    #[serde(default)]
    pub allow_full_autonomy_bypass: bool,
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
    /// If true, even agents with Full autonomy must ask for approval.
    /// Requires `allow_full_autonomy_bypass = false` (the default) to take effect.
    #[serde(default)]
    pub always_ask: bool,
    /// If true, only clients matching `approval.admin_cn_pattern` may submit the approval token.
    #[serde(default)]
    pub approval_admin_only: bool,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: default_rate_limit_enabled(),
            max_requests: default_rate_limit_max(),
            window_seconds: default_rate_limit_window(),
            endpoints: default_rate_limited_endpoints(),
        }
    }
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
                admin_cn_pattern: None,
                identity_plugin: None,
            },
            metrics: MetricsConfig {
                enabled: true,
                bind: "127.0.0.1:19090".to_string(),
            },
            rate_limit: RateLimitConfig::default(),
            agents: vec![
                AgentConfig {
                    cn_pattern: "librarian*".to_string(),
                    agent_type: "librarian".to_string(),
                    unix_user: "librarian".to_string(),
                    autonomy: AutonomyLevel::Supervised,
                    allowed_operations: vec!["zfs-list".to_string(), "zfs-snapshot".to_string()],
                    requires_approval_for: vec!["zfs-destroy".to_string()],
                    pattern_rules: vec![],
                    allow_full_autonomy_bypass: false,
                },
                AgentConfig {
                    cn_pattern: "claude-code*".to_string(),
                    agent_type: "acp_harness".to_string(),
                    unix_user: "clash-agent".to_string(),
                    autonomy: AutonomyLevel::Supervised,
                    allowed_operations: vec!["zfs-list".to_string()],
                    requires_approval_for: vec!["zfs-snapshot".to_string(), "zfs-destroy".to_string()],
                    pattern_rules: vec![],
                    allow_full_autonomy_bypass: false,
                },
            ],
            rules: vec![
                // ZFS rules
                RuleConfig {
                    operation: "zfs-destroy".to_string(),
                    approval_required: true,
                    pattern: None,
                    always_ask: true,        // destroy always requires approval
                    approval_admin_only: false,
                },
                RuleConfig {
                    operation: "zfs-snapshot".to_string(),
                    approval_required: false,
                    pattern: None,
                    always_ask: false,
                    approval_admin_only: false,
                },
                RuleConfig {
                    operation: "zfs-list".to_string(),
                    approval_required: false,
                    pattern: None,
                    always_ask: false,
                    approval_admin_only: false,
                },
                RuleConfig {
                    operation: "zfs-rollback".to_string(),
                    approval_required: true,
                    pattern: None,
                    always_ask: true,
                    approval_admin_only: false,
                },
                // Systemd rules
                RuleConfig {
                    operation: "systemd-status".to_string(),
                    approval_required: false,
                    pattern: None,
                    always_ask: false,
                    approval_admin_only: false,
                },
                RuleConfig {
                    operation: "systemd-start".to_string(),
                    approval_required: false,
                    pattern: None,
                    always_ask: false,
                    approval_admin_only: false,
                },
                RuleConfig {
                    operation: "systemd-stop".to_string(),
                    approval_required: false,
                    pattern: None,
                    always_ask: false,
                    approval_admin_only: false,
                },
                RuleConfig {
                    operation: "systemd-restart".to_string(),
                    approval_required: false,
                    pattern: None,
                    always_ask: false,
                    approval_admin_only: false,
                },
                // PCT rules
                RuleConfig {
                    operation: "pct-status".to_string(),
                    approval_required: false,
                    pattern: None,
                    always_ask: false,
                    approval_admin_only: false,
                },
                RuleConfig {
                    operation: "pct-start".to_string(),
                    approval_required: true,
                    pattern: None,
                    always_ask: false,
                    approval_admin_only: false,
                },
                RuleConfig {
                    operation: "pct-stop".to_string(),
                    approval_required: true,
                    pattern: None,
                    always_ask: false,
                    approval_admin_only: false,
                },
                RuleConfig {
                    operation: "pct-destroy".to_string(),
                    approval_required: true,
                    pattern: None,
                    always_ask: true,
                    approval_admin_only: true,
                },
                // Git rules
                RuleConfig {
                    operation: "git-status".to_string(),
                    approval_required: false,
                    pattern: None,
                    always_ask: false,
                    approval_admin_only: false,
                },
                RuleConfig {
                    operation: "git-log".to_string(),
                    approval_required: false,
                    pattern: None,
                    always_ask: false,
                    approval_admin_only: false,
                },
                RuleConfig {
                    operation: "git-fetch".to_string(),
                    approval_required: false,
                    pattern: None,
                    always_ask: false,
                    approval_admin_only: false,
                },
                RuleConfig {
                    operation: "git-pull".to_string(),
                    approval_required: false,
                    pattern: None,
                    always_ask: false,
                    approval_admin_only: false,
                },
                RuleConfig {
                    operation: "git-checkout".to_string(),
                    approval_required: true,
                    pattern: None,
                    always_ask: false,
                    approval_admin_only: false,
                },
            ],
            git: Some(GitConfig {
                allowed_repos: vec!["/srv".to_string(), "/opt".to_string()],
            }),
            exec: Some(ExecConfig {
                enabled: false, // disabled by default
                allowed_commands: vec![],
                ansible_job_queue: None,
            }),
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

    /// Check if an operation requires approval (P0-4 / P-B4)
    ///
    /// When `agent` is provided, Full-autonomy bypass logic is applied:
    /// - If the rule has `always_ask = true`, approval is ALWAYS required regardless of autonomy.
    /// - Otherwise, if the agent has `autonomy = Full` AND `allow_full_autonomy_bypass = true`,
    ///   the approval requirement is bypassed.
    /// - Default: Full autonomy does NOT bypass approval (safe default, P-B4).
    pub fn requires_approval(&self, operation: &str, target: &str) -> bool {
        self.requires_approval_for_agent(operation, target, None)
    }

    /// Like `requires_approval` but aware of the calling agent's autonomy level.
    pub fn requires_approval_for_agent(
        &self,
        operation: &str,
        target: &str,
        agent: Option<&AgentConfig>,
    ) -> bool {
        let rule = match self.rules.iter().find(|r| r.operation == operation) {
            Some(r) => r,
            None => return false,
        };

        if !rule.approval_required {
            return false;
        }

        // Check pattern if present
        if let Some(ref pattern) = rule.pattern {
            if let Ok(re) = regex::Regex::new(pattern) {
                if !re.is_match(target) {
                    return false;
                }
            }
        }

        // `always_ask = true` overrides Full autonomy bypass entirely (P-B4)
        if rule.always_ask {
            return true;
        }

        // Full-autonomy bypass: only if the agent explicitly opts in AND the rule allows it
        if let Some(agent) = agent {
            if agent.autonomy == AutonomyLevel::Full && agent.allow_full_autonomy_bypass {
                return false;
            }
        }

        true
    }

    /// Return the rule for a given operation if it exists.
    pub fn find_rule(&self, operation: &str) -> Option<&RuleConfig> {
        self.rules.iter().find(|r| r.operation == operation)
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
#[derive(Clone)]
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
        // AutonomyLevel is used as the value for the `autonomy` field in AgentConfig.
        // Test via a wrapper struct to mimic TOML deserialization.
        #[derive(serde::Deserialize)]
        struct Wrapper { autonomy: AutonomyLevel }

        let w: Wrapper = toml::from_str(r#"autonomy = "supervised""#).unwrap();
        assert_eq!(w.autonomy, AutonomyLevel::Supervised);

        let w: Wrapper = toml::from_str(r#"autonomy = "full""#).unwrap();
        assert_eq!(w.autonomy, AutonomyLevel::Full);

        let w: Wrapper = toml::from_str(r#"autonomy = "read_only""#).unwrap();
        assert_eq!(w.autonomy, AutonomyLevel::ReadOnly);
    }

    /// P-B4: Full autonomy cannot bypass always_ask = true operations (default safe)
    #[test]
    fn test_full_autonomy_cannot_bypass_always_ask() {
        let config = Config::default();
        
        // zfs-destroy has always_ask = true in default rules
        let full_agent = AgentConfig {
            cn_pattern: "full-agent*".to_string(),
            agent_type: "test".to_string(),
            unix_user: "test".to_string(),
            autonomy: AutonomyLevel::Full,
            allowed_operations: vec![],
            requires_approval_for: vec![],
            pattern_rules: vec![],
            allow_full_autonomy_bypass: true, // even with bypass enabled
        };

        // always_ask=true overrides even allow_full_autonomy_bypass=true
        assert!(
            config.requires_approval_for_agent("zfs-destroy", "tank/media", Some(&full_agent)),
            "Full autonomy with bypass should NOT bypass always_ask=true operations"
        );
    }

    /// P-B4: Full autonomy CAN bypass when always_ask=false and bypass is explicitly enabled
    #[test]
    fn test_full_autonomy_bypass_when_explicitly_enabled() {
        let mut config = Config::default();
        // Set snapshot rule: approval required, but always_ask=false
        config.rules.push(RuleConfig {
            operation: "zfs-snapshot-protected".to_string(),
            approval_required: true,
            pattern: None,
            always_ask: false,
            approval_admin_only: false,
        });

        let full_agent_with_bypass = AgentConfig {
            cn_pattern: "full-agent*".to_string(),
            agent_type: "test".to_string(),
            unix_user: "test".to_string(),
            autonomy: AutonomyLevel::Full,
            allowed_operations: vec![],
            requires_approval_for: vec![],
            pattern_rules: vec![],
            allow_full_autonomy_bypass: true,
        };

        let full_agent_no_bypass = AgentConfig {
            allow_full_autonomy_bypass: false,
            ..full_agent_with_bypass.clone()
        };

        // With bypass enabled: skip approval
        assert!(
            !config.requires_approval_for_agent(
                "zfs-snapshot-protected", "tank/data", Some(&full_agent_with_bypass)
            ),
            "Full autonomy with bypass=true should skip approval when always_ask=false"
        );

        // Without bypass: still require approval
        assert!(
            config.requires_approval_for_agent(
                "zfs-snapshot-protected", "tank/data", Some(&full_agent_no_bypass)
            ),
            "Full autonomy with bypass=false should still require approval"
        );
    }

    /// P-B4: Supervised autonomy always requires approval regardless of bypass flag
    #[test]
    fn test_supervised_always_requires_approval() {
        let config = Config::default();

        let supervised_agent = AgentConfig {
            cn_pattern: "supervised*".to_string(),
            agent_type: "test".to_string(),
            unix_user: "test".to_string(),
            autonomy: AutonomyLevel::Supervised,
            allowed_operations: vec![],
            requires_approval_for: vec![],
            pattern_rules: vec![],
            allow_full_autonomy_bypass: true, // irrelevant for Supervised
        };

        assert!(
            config.requires_approval_for_agent("zfs-destroy", "tank/media", Some(&supervised_agent)),
            "Supervised agent should require approval"
        );
    }
}
