//! Agent adapter framework for mapping certificates to agent identities (P3-17)
//!
//! This module provides adapters for different agent types:
//! - Librarian: Brian's primary agent
//! - Lucien: Infrastructure guardian
//! - Zeroclaw: NZC CLI agent
//! - ACPX: Anthropic Computer Protocol eXtended agents (Codex, Claude Code, etc.)

use crate::config::AgentConfig;

/// Registry of agent adapters
pub struct AgentRegistry {
    configs: Vec<AgentConfig>,
}

impl AgentRegistry {
    pub fn new(configs: Vec<AgentConfig>) -> Self {
        Self {
            configs,
        }
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
    use crate::config::AutonomyLevel;
    use std::collections::HashMap;
    use serde::{Deserialize, Serialize};

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
                "zeroclaw" | "nzc" | "nonzeroclaw" => AgentType::Zeroclaw,
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
            if let Some(pattern) = self.pattern_rules.get(operation) {
                if let Ok(re) = regex::Regex::new(pattern) {
                    if re.is_match(target) {
                        return true;
                    }
                }
            }
            true
        }
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
