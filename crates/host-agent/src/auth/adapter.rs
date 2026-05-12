//! Agent adapter framework for mapping certificates to agent identities (P3-17)
//!
//! This module provides adapters for different agent types:
//! - Generic: a caller without a product-specific adapter
//! - Zeroclaw: ZeroClaw CLI agent
//! - ACPX: Anthropic Computer Protocol eXtended agents (Codex, Claude Code, etc.)

use crate::config::{AgentConfig, cn_pattern_matches};

/// Registry of agent adapters
pub struct AgentRegistry {
    configs: Vec<AgentConfig>,
}

impl AgentRegistry {
    pub fn new(configs: Vec<AgentConfig>) -> Self {
        Self { configs }
    }

    /// Return true when a certificate CN is explicitly configured as an agent.
    pub fn is_registered(&self, cn: &str) -> bool {
        self.configs
            .iter()
            .any(|config| cn_pattern_matches(&config.cn_pattern, cn))
    }

    /// Return a placeholder CN for policy lookups when no per-request identity is available.
    /// Returns None if no configs are registered.
    pub fn resolve_cn_placeholder(&self) -> Option<String> {
        // Return the first registered CN pattern (stripped of '*') as a placeholder
        self.configs
            .first()
            .map(|c| c.cn_pattern.trim_end_matches('*').to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::AgentRegistry;
    use crate::config::{AgentConfig, AutonomyLevel};
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;

    /// Supported agent types
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
    enum AgentType {
        Librarian,
        Lucien,
        Zeroclaw,
        AcpHarness,
        Custom(&'static str),
    }

    impl AgentType {
        fn from_str(s: &str) -> Self {
            match s.to_lowercase().as_str() {
                "librarian" => AgentType::Librarian,
                "lucien" => AgentType::Lucien,
                "zeroclaw" => AgentType::Zeroclaw,
                "acp" | "acpx" | "acp_harness" | "claude-code" | "codex" => AgentType::AcpHarness,
                other => AgentType::Custom(Box::leak(other.to_string().into_boxed_str())),
            }
        }
    }

    /// Policy profile for an agent
    #[derive(Debug, Clone)]
    struct PolicyProfile {
        autonomy_level: AutonomyLevel,
        auto_approve: Vec<String>,
        always_ask: Vec<String>,
        pattern_rules: HashMap<String, String>,
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
        fn requires_approval(&self, operation: &str, target: &str) -> bool {
            if self.autonomy_level == AutonomyLevel::Full {
                return false;
            }
            if self.autonomy_level == AutonomyLevel::ReadOnly {
                return true;
            }
            if self.always_ask.contains(&operation.to_string()) {
                return true;
            }
            if self.auto_approve.contains(&operation.to_string()) {
                return false;
            }
            if let Some(pattern) = self.pattern_rules.get(operation)
                && let Ok(re) = regex::Regex::new(pattern)
                && re.is_match(target)
            {
                return true;
            }
            true
        }
    }

    fn agent_config(cn_pattern: &str) -> AgentConfig {
        AgentConfig {
            cn_pattern: cn_pattern.to_string(),
            agent_type: "generic".to_string(),
            unix_user: "clash-agent".to_string(),
            autonomy: AutonomyLevel::Supervised,
            allowed_operations: vec![],
            requires_approval_for: vec![],
            pattern_rules: vec![],
            allow_full_autonomy_bypass: false,
        }
    }

    #[test]
    fn agent_registry_rejects_unconfigured_cn() {
        let registry = AgentRegistry::new(vec![agent_config("librarian*"), agent_config("admin")]);

        assert!(registry.is_registered("librarian-main"));
        assert!(registry.is_registered("admin"));
        assert!(!registry.is_registered("nobody"));
    }

    #[test]
    fn agent_registry_rejects_global_wildcard_cn_pattern() {
        let registry = AgentRegistry::new(vec![agent_config("*")]);

        assert!(!registry.is_registered("any-agent"));
        assert!(!registry.is_registered(""));
    }

    #[test]
    fn test_agent_type_from_str() {
        assert!(matches!(
            AgentType::from_str("librarian"),
            AgentType::Librarian
        ));
        assert!(matches!(
            AgentType::from_str("codex"),
            AgentType::AcpHarness
        ));
        assert!(matches!(
            AgentType::from_str("claude-code"),
            AgentType::AcpHarness
        ));
    }

    #[test]
    fn test_policy_requires_approval() {
        let mut profile = PolicyProfile::default();
        assert!(profile.requires_approval("zfs-destroy", "tank/media"));
        profile.auto_approve.push("zfs-snapshot".to_string());
        assert!(!profile.requires_approval("zfs-snapshot", "tank/media"));
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
        assert!(!profile.requires_approval("zfs-destroy", "tank/media"));
    }
}
