//! Agent adapter framework for mapping certificates to agent identities (P3-17)
//!
//! This module provides adapters for different agent types:
//! - Librarian: Brian's primary agent
//! - Lucien: Infrastructure guardian
//! - Zeroclaw: NZC CLI agent
//! - ACPX: Anthropic Computer Protocol eXtended agents (Codex, Claude Code, etc.)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

use super::identity::{ClientIdentity, resolve_unix_user};
use crate::config::{AgentConfig, AutonomyLevel};

/// Supported agent types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub enum AgentType {
    Librarian,
    Lucien,
    Zeroclaw,
    AcpHarness,
    Custom(&'static str),
}

impl std::fmt::Display for AgentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentType::Librarian => write!(f, "librarian"),
            AgentType::Lucien => write!(f, "lucien"),
            AgentType::Zeroclaw => write!(f, "zeroclaw"),
            AgentType::AcpHarness => write!(f, "acp_harness"),
            AgentType::Custom(s) => write!(f, "{}", s),
        }
    }
}

impl AgentType {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "librarian" => AgentType::Librarian,
            "lucien" => AgentType::Lucien,
            "zeroclaw" | "nzc" | "nonzeroclaw" => AgentType::Zeroclaw,
            "acp" | "acpx" | "acp_harness" | "claude-code" | "codex" => AgentType::AcpHarness,
            other => AgentType::Custom(Box::leak(other.to_string().into_boxed_str())),
        }
    }
}

/// Agent identity with type and instance information
#[derive(Debug, Clone)]
pub struct AgentIdentity {
    pub agent_type: AgentType,
    /// Instance name (e.g., "main", "coding-session-abc", "review-agent")
    pub instance: String,
    /// Original certificate CN
    pub cert_cn: String,
    /// Resolved Unix UID
    pub uid: u32,
    /// Unix username
    pub unix_user: String,
    /// Policy profile for this agent
    pub policy: PolicyProfile,
}

/// Policy profile for an agent
#[derive(Debug, Clone)]
pub struct PolicyProfile {
    pub autonomy_level: AutonomyLevel,
    /// Operations this agent can perform without approval
    pub auto_approve: Vec<String>,
    /// Operations that always require approval regardless of autonomy
    pub always_ask: Vec<String>,
    /// Pattern-based rules (operation -> regex pattern)
    pub pattern_rules: HashMap<String, String>,
}

impl Default for PolicyProfile {
    fn default() -> Self {
        Self {
            autonomy_level: AutonomyLevel::Supervised,
            auto_approve: vec!["zfs-list".to_string()],
            always_ask: vec!["zfs-destroy".to_string()],
            pattern_rules: HashMap::new(),
        }
    }
}

impl PolicyProfile {
    /// Check if an operation requires approval
    pub fn requires_approval(&self, operation: &str, target: &str) -> bool {
        // Full autonomy = never prompt
        if self.autonomy_level == AutonomyLevel::Full {
            return false;
        }

        // ReadOnly = block everything (handled elsewhere)
        if self.autonomy_level == AutonomyLevel::ReadOnly {
            return true; // Will be denied, not approved
        }

        // always_ask overrides everything
        if self.always_ask.contains(&operation.to_string()) {
            return true;
        }

        // auto_approve skips prompt
        if self.auto_approve.contains(&operation.to_string()) {
            return false;
        }

        // Check pattern rules
        if let Some(pattern) = self.pattern_rules.get(operation) {
            if let Ok(re) = regex::Regex::new(pattern) {
                if re.is_match(target) {
                    return true; // Pattern match requires approval
                }
            }
        }

        // Default: supervised mode requires approval
        true
    }
}

/// Adapter for mapping CN to agent identity
pub trait AgentAdapter: Send + Sync {
    /// Try to identify an agent from certificate CN
    fn identify(&self, cn: &str, configs: &[AgentConfig]) -> Option<AgentIdentity>;
}

/// Default adapter using config-based pattern matching
pub struct ConfigAgentAdapter;

impl AgentAdapter for ConfigAgentAdapter {
    fn identify(&self, cn: &str, configs: &[AgentConfig]) -> Option<AgentIdentity> {
        // Find matching config
        let config = configs.iter().find(|c| {
            // Simple glob-style matching: librarian* matches librarian, librarian-main, etc.
            if c.cn_pattern.ends_with('*') {
                let prefix = &c.cn_pattern[..c.cn_pattern.len()-1];
                cn.starts_with(prefix)
            } else {
                c.cn_pattern == cn
            }
        })?;

        // Resolve Unix user
        let (unix_user, uid) = resolve_unix_user(&config.unix_user)
            .or_else(|_| resolve_unix_user(cn))
            .ok()?;

        // Build instance name from CN (e.g., "librarian-main" -> "main")
        let instance = if cn.starts_with(&config.cn_pattern.trim_end_matches('*')) {
            let prefix_len = config.cn_pattern.trim_end_matches('*').len();
            if cn.len() > prefix_len {
                cn[prefix_len..].trim_start_matches('-').to_string()
            } else {
                "main".to_string()
            }
        } else {
            "main".to_string()
        };

        let agent_type = AgentType::from_str(&config.agent_type);

        let policy = PolicyProfile {
            autonomy_level: config.autonomy.clone(),
            auto_approve: config.allowed_operations.clone(),
            always_ask: config.requires_approval_for.clone(),
            pattern_rules: config.pattern_rules.iter()
                .map(|r| (r.operation.clone(), r.pattern.clone()))
                .collect(),
        };

        Some(AgentIdentity {
            agent_type,
            instance,
            cert_cn: cn.to_string(),
            uid,
            unix_user,
            policy,
        })
    }
}

/// Registry of agent adapters
pub struct AgentRegistry {
    adapter: Arc<dyn AgentAdapter>,
    configs: Vec<AgentConfig>,
    // Cache of resolved identities
    cache: std::sync::Mutex<HashMap<String, AgentIdentity>>,
}

impl AgentRegistry {
    pub fn new(configs: Vec<AgentConfig>) -> Self {
        Self {
            adapter: Arc::new(ConfigAgentAdapter),
            configs,
            cache: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Register a custom adapter (for testing or extensions)
    pub fn with_adapter(adapter: Arc<dyn AgentAdapter>, configs: Vec<AgentConfig>) -> Self {
        Self {
            adapter,
            configs,
            cache: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Identify an agent from CN
    pub fn identify(&self, cn: &str) -> Option<AgentIdentity> {
        // Check cache first
        if let Ok(cache) = self.cache.lock() {
            if let Some(identity) = cache.get(cn) {
                debug!(cn = %cn, "Agent identity cache hit");
                return Some(identity.clone());
            }
        }

        // Resolve via adapter
        if let Some(identity) = self.adapter.identify(cn, &self.configs) {
            // Cache the result
            if let Ok(mut cache) = self.cache.lock() {
                cache.insert(cn.to_string(), identity.clone());
            }
            info!(cn = %cn, agent_type = %identity.agent_type, uid = %identity.uid, "Identified agent");
            return Some(identity);
        }

        warn!(cn = %cn, "Failed to identify agent");
        None
    }

    /// Clear identity cache (e.g., after config reload)
    pub fn clear_cache(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
            info!("Agent identity cache cleared");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> AgentConfig {
        AgentConfig {
            cn_pattern: "librarian*".to_string(),
            agent_type: "librarian".to_string(),
            unix_user: "librarian".to_string(),
            autonomy: AutonomyLevel::Supervised,
            allowed_operations: vec!["zfs-list".to_string()],
            requires_approval_for: vec!["zfs-destroy".to_string()],
            pattern_rules: vec![],
        }
    }

    #[test]
    fn test_agent_type_from_str() {
        assert!(matches!(AgentType::from_str("librarian"), AgentType::Librarian));
        assert!(matches!(AgentType::from_str("codex"), AgentType::AcpHarness));
        assert!(matches!(AgentType::from_str("claude-code"), AgentType::AcpHarness));
    }

    #[test]
    fn test_policy_requires_approval() {
        let mut profile = PolicyProfile::default();
        
        // Default: supervised requires approval
        assert!(profile.requires_approval("zfs-destroy", "tank/media"));
        
        // auto_approve skips approval
        profile.auto_approve.push("zfs-snapshot".to_string());
        assert!(!profile.requires_approval("zfs-snapshot", "tank/media"));
        
        // always_ask overrides auto_approve
        profile.always_ask.push("zfs-snapshot".to_string());
        assert!(profile.requires_approval("zfs-snapshot", "tank/media"));
    }

    #[test]
    fn test_full_autonomy_never_requires_approval() {
        let profile = PolicyProfile {
            autonomy_level: AutonomyLevel::Full,
            always_ask: vec!["zfs-destroy".to_string()],
            ..Default::default()
        };
        
        // Full autonomy should not require approval even for always_ask
        // (though these will be handled differently at the policy engine level)
        assert!(!profile.requires_approval("zfs-destroy", "tank/media"));
    }
}
