//! Authentication and authorization for proxy requests.
//!
//! Handles API key validation, agent identification, and model access control.

use tracing::{debug, warn};

use crate::config::{ProxyAccessPolicy, ProxyConfig};

/*
/// Authenticated agent information.
#[derive(Debug, Clone)]
pub struct AuthContext {
    /// Agent ID (e.g., "lucien", "claude-code")
    pub agent_id: String,
    /// Display name
    pub agent_name: String,
}
*/

/// Check if a model is allowed for a given agent.
pub fn check_model_access(config: &ProxyConfig, agent_id: &str, model: &str) -> bool {
    check_model_access_for_names(config, agent_id, model, model)
}

/// Check access when a client-facing model name may resolve to a different
/// backend model name. Block rules apply to either name; allow rules can match
/// either the requested alias or the resolved canonical model.
pub fn check_model_access_for_names(
    config: &ProxyConfig,
    agent_id: &str,
    requested_model: &str,
    resolved_model: &str,
) -> bool {
    // Find agent configuration
    let agent_config = config.agents.iter().find(|a| a.id == agent_id);

    match agent_config {
        Some(agent) => {
            // Check blocked models first (takes precedence)
            for pattern in &agent.blocked_models {
                if model_matches(requested_model, pattern) || model_matches(resolved_model, pattern)
                {
                    debug!(agent_id = %agent_id, requested_model = %requested_model, resolved_model = %resolved_model, pattern = %pattern, "Model blocked");
                    return false;
                }
            }

            // Check allowed models
            if agent.allowed_models.is_empty() {
                // No specific allowed models = allow all (except blocked)
                true
            } else {
                // Must match at least one allowed pattern
                let allowed = agent.allowed_models.iter().any(|pattern| {
                    model_matches(requested_model, pattern)
                        || model_matches(resolved_model, pattern)
                });
                debug!(agent_id = %agent_id, requested_model = %requested_model, resolved_model = %resolved_model, allowed = allowed, "Checked model access");
                allowed
            }
        }
        None => {
            // No agent config found - use default policy
            match config.default_policy {
                ProxyAccessPolicy::AllowAll => true,
                ProxyAccessPolicy::DenyAll => false,
                ProxyAccessPolicy::AllowConfigured => {
                    warn!(agent_id = %agent_id, "Agent not configured and policy is AllowConfigured");
                    false
                }
            }
        }
    }
}

/*
/// Validate API key against global key or agent-specific keys.
pub fn validate_api_key(config: &ProxyConfig, key: &str) -> Option<String> {
    // Check global API key first
    if let Some(global_key) = &config.api_key {
        if constant_time_eq(key, global_key) {
            return Some("global".to_string());
        }
    }

    // Check agent-specific keys
    for agent in &config.agents {
        if let Some(agent_key) = &agent.api_key {
            if constant_time_eq(key, agent_key) {
                return Some(agent.id.clone());
            }
        }
    }

    None
}
*/

/// Check if a model matches a pattern (supports wildcards).
fn model_matches(model: &str, pattern: &str) -> bool {
    if pattern == "*" {
        // Universal wildcard: matches everything
        true
    } else if pattern.ends_with("/*") {
        // Prefix match: "deepseek/*" matches "deepseek-chat" and "deepseek-reasoner"
        // "kimi/*" matches "kimi/kimi-for-coding" and "kimi/kimi-lite"
        // Remove the "/*" to get the prefix
        let prefix = pattern.strip_suffix("/*").unwrap_or(pattern);
        model.starts_with(prefix)
    } else {
        // Exact match
        model == pattern
    }
}

/*
/// Constant-time string comparison to prevent timing attacks.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();

    if a_bytes.len() != b_bytes.len() {
        return false;
    }

    let mut result = 0u8;
    for (x, y) in a_bytes.iter().zip(b_bytes.iter()) {
        result |= x ^ y;
    }

    result == 0
}
*/

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_matches_exact() {
        assert!(model_matches(
            "kimi/kimi-for-coding",
            "kimi/kimi-for-coding"
        ));
        assert!(!model_matches("kimi/kimi-for-coding", "kimi/kimi-lite"));
    }

    #[test]
    fn test_model_matches_wildcard() {
        assert!(model_matches("kimi/kimi-for-coding", "kimi/*"));
        assert!(model_matches("kimi/kimi-lite", "kimi/*"));
        assert!(!model_matches("deepseek/deepseek-chat", "kimi/*"));
    }

    #[test]
    fn test_model_matches_wildcard_star() {
        // "*" should match any model
        assert!(
            model_matches("deepseek-chat", "*"),
            "Expected '*' to match 'deepseek-chat'"
        );
        assert!(
            model_matches("kimi/kimi-for-coding", "*"),
            "Expected '*' to match 'kimi/kimi-for-coding'"
        );
        assert!(
            model_matches("test-alloy", "*"),
            "Expected '*' to match 'test-alloy'"
        );
    }

    #[test]
    fn test_check_model_access_allow_all() {
        let config = ProxyConfig {
            enabled: true,
            bind: "127.0.0.1:8083".to_string(),
            api_key: None,
            api_key_file: None,
            timeout_seconds: 300,
            max_body_mb: 50,
            default_policy: ProxyAccessPolicy::AllowAll,
            backend_type: "http".to_string(),
            backend_url: "https://api.deepseek.com/v1".to_string(),
            backend_api_key: None,
            headers: None,
            backend_api_key_file: None,
            providers: vec![],
            model_routes: vec![],
            token_estimator: Default::default(),
            agents: vec![],
            voice: None,
            ..Default::default()
        };

        // With AllowAll policy, any model should be allowed
        assert!(check_model_access(&config, "test-agent", "deepseek-chat"));
        assert!(check_model_access(&config, "test-agent", "any-model"));
        assert!(check_model_access(
            &config,
            "non-existent-agent",
            "deepseek-chat"
        ));
    }

    #[test]
    fn test_check_model_access_deny_all() {
        let config = ProxyConfig {
            enabled: true,
            bind: "127.0.0.1:8083".to_string(),
            api_key: None,
            api_key_file: None,
            timeout_seconds: 300,
            max_body_mb: 50,
            default_policy: ProxyAccessPolicy::DenyAll,
            backend_type: "http".to_string(),
            backend_url: "https://api.deepseek.com/v1".to_string(),
            backend_api_key: None,
            headers: None,
            backend_api_key_file: None,
            providers: vec![],
            model_routes: vec![],
            token_estimator: Default::default(),
            agents: vec![],
            voice: None,
            ..Default::default()
        };

        // With DenyAll policy, no models should be allowed
        assert!(!check_model_access(&config, "test-agent", "deepseek-chat"));
        assert!(!check_model_access(&config, "any-agent", "any-model"));
    }

    #[test]
    fn test_check_model_access_agent_specific() {
        use crate::config::ProxyAgentConfig;

        let config = ProxyConfig {
            enabled: true,
            bind: "127.0.0.1:8083".to_string(),
            api_key: None,
            api_key_file: None,
            timeout_seconds: 300,
            max_body_mb: 50,
            default_policy: ProxyAccessPolicy::AllowConfigured,
            backend_type: "http".to_string(),
            backend_url: "https://api.deepseek.com/v1".to_string(),
            backend_api_key: None,
            headers: None,
            backend_api_key_file: None,
            providers: vec![],
            model_routes: vec![],
            token_estimator: Default::default(),
            agents: vec![ProxyAgentConfig {
                id: "test-agent".to_string(),
                name: Some("Test Agent".to_string()),
                api_key: Some("test-key".to_string()),
                api_key_file: None,
                allowed_models: vec!["deepseek/*".to_string(), "test-alloy".to_string()],
                blocked_models: vec![],
                rate_limit_rpm: 0,
                rate_limit_tpm: 0,
            }],
            voice: None,
            ..Default::default()
        };

        // Agent should have access to allowed models
        assert!(check_model_access(&config, "test-agent", "deepseek-chat"));
        assert!(check_model_access(
            &config,
            "test-agent",
            "deepseek-reasoner"
        ));
        assert!(check_model_access(&config, "test-agent", "test-alloy"));

        // Agent should NOT have access to other models
        assert!(!check_model_access(
            &config,
            "test-agent",
            "kimi/kimi-for-coding"
        ));

        // Other agents should be denied (not in configured list)
        assert!(!check_model_access(&config, "other-agent", "deepseek-chat"));
    }

    #[test]
    fn test_check_model_access_blocked_models() {
        use crate::config::ProxyAgentConfig;

        let config = ProxyConfig {
            enabled: true,
            bind: "127.0.0.1:8083".to_string(),
            api_key: None,
            api_key_file: None,
            timeout_seconds: 300,
            max_body_mb: 50,
            default_policy: ProxyAccessPolicy::AllowConfigured,
            backend_type: "http".to_string(),
            backend_url: "https://api.deepseek.com/v1".to_string(),
            backend_api_key: None,
            headers: None,
            backend_api_key_file: None,
            providers: vec![],
            model_routes: vec![],
            token_estimator: Default::default(),
            agents: vec![ProxyAgentConfig {
                id: "test-agent".to_string(),
                name: Some("Test Agent".to_string()),
                api_key: Some("test-key".to_string()),
                api_key_file: None,
                allowed_models: vec!["*".to_string()], // Allow all
                blocked_models: vec!["dangerous-model".to_string(), "secret/*".to_string()],
                rate_limit_rpm: 0,
                rate_limit_tpm: 0,
            }],
            voice: None,
            ..Default::default()
        };

        // Blocked models should be denied even if allowed_models says "*"
        assert!(!check_model_access(
            &config,
            "test-agent",
            "dangerous-model"
        ));
        assert!(!check_model_access(&config, "test-agent", "secret/model-a"));

        // Other models should be allowed
        assert!(check_model_access(&config, "test-agent", "safe-model"));
    }

    #[test]
    fn test_check_model_access_for_resolved_names_allows_alias_or_canonical() {
        use crate::config::ProxyAgentConfig;

        let config = ProxyConfig {
            default_policy: ProxyAccessPolicy::AllowConfigured,
            agents: vec![ProxyAgentConfig {
                id: "test-agent".to_string(),
                name: Some("Test Agent".to_string()),
                api_key: None,
                api_key_file: None,
                allowed_models: vec!["local-dispatcher".to_string(), "kimi/*".to_string()],
                blocked_models: vec![],
                rate_limit_rpm: 0,
                rate_limit_tpm: 0,
            }],
            ..Default::default()
        };

        assert!(check_model_access_for_names(
            &config,
            "test-agent",
            "local-dispatcher",
            "qwen-test:small"
        ));
        assert!(check_model_access_for_names(
            &config,
            "test-agent",
            "balanced",
            "kimi/kimi-test:medium"
        ));
        assert!(!check_model_access_for_names(
            &config,
            "test-agent",
            "balanced",
            "codex/gpt-5.5"
        ));
    }

    #[test]
    fn test_check_model_access_for_resolved_names_blocks_alias_or_canonical() {
        use crate::config::ProxyAgentConfig;

        let config = ProxyConfig {
            default_policy: ProxyAccessPolicy::AllowConfigured,
            agents: vec![ProxyAgentConfig {
                id: "test-agent".to_string(),
                name: Some("Test Agent".to_string()),
                api_key: None,
                api_key_file: None,
                allowed_models: vec!["*".to_string()],
                blocked_models: vec!["local-dispatcher".to_string(), "secret/*".to_string()],
                rate_limit_rpm: 0,
                rate_limit_tpm: 0,
            }],
            ..Default::default()
        };

        assert!(!check_model_access_for_names(
            &config,
            "test-agent",
            "local-dispatcher",
            "qwen-test:small"
        ));
        assert!(!check_model_access_for_names(
            &config,
            "test-agent",
            "balanced",
            "secret/model"
        ));
        assert!(check_model_access_for_names(
            &config,
            "test-agent",
            "balanced",
            "qwen-test:small"
        ));
    }

    /*
    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq("secret", "secret"));
        assert!(!constant_time_eq("secret", "secrets"));
        assert!(!constant_time_eq("secret", "secreT"));
    }
    */
}
