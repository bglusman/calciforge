// Unit tests for auth module
// These are the kinds of tests mutation testing works against

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PolyConfig;
    use crate::proxy::auth::{check_model_access, model_matches, ProxyAccessPolicy};

    #[test]
    fn test_model_matches_exact() {
        // Exact match should work
        assert!(model_matches("deepseek-chat", "deepseek-chat"));
        assert!(model_matches("kimi/kimi-for-coding", "kimi/kimi-for-coding"));
        
        // Different models should not match
        assert!(!model_matches("deepseek-chat", "deepseek-reasoner"));
        assert!(!model_matches("model-a", "model-b"));
    }

    #[test]
    fn test_model_matches_wildcard_prefix() {
        // Prefix wildcards should work
        assert!(model_matches("deepseek-chat", "deepseek/*"));
        assert!(model_matches("deepseek-reasoner", "deepseek/*"));
        assert!(model_matches("kimi/kimi-for-coding", "kimi/*"));
        
        // Should not match wrong prefix
        assert!(!model_matches("deepseek-chat", "kimi/*"));
        assert!(!model_matches("openai/gpt-4", "anthropic/*"));
    }

    #[test]
    fn test_model_matches_wildcard_star() {
        // TODO: This test will FAIL with current implementation
        // "*" should match any model
        assert!(model_matches("deepseek-chat", "*"), 
            "Expected '*' to match 'deepseek-chat'");
        assert!(model_matches("kimi/kimi-for-coding", "*"),
            "Expected '*' to match 'kimi/kimi-for-coding'");
        assert!(model_matches("test-alloy", "*"),
            "Expected '*' to match 'test-alloy'");
    }

    #[test]
    fn test_check_model_access_allow_all() {
        let mut config = PolyConfig::default();
        config.proxy.default_policy = ProxyAccessPolicy::AllowAll;
        
        // With AllowAll policy, any model should be allowed
        assert!(check_model_access(&config, "test-agent", "deepseek-chat").is_ok());
        assert!(check_model_access(&config, "test-agent", "any-model").is_ok());
        assert!(check_model_access(&config, "non-existent-agent", "deepseek-chat").is_ok());
    }

    #[test]
    fn test_check_model_access_deny_all() {
        let mut config = PolyConfig::default();
        config.proxy.default_policy = ProxyAccessPolicy::DenyAll;
        
        // With DenyAll policy, no models should be allowed
        assert!(check_model_access(&config, "test-agent", "deepseek-chat").is_err());
        assert!(check_model_access(&config, "any-agent", "any-model").is_err());
    }

    #[test]
    fn test_check_model_access_agent_specific() {
        let mut config = PolyConfig::default();
        config.proxy.default_policy = ProxyAccessPolicy::AllowConfigured;
        
        // Add a test agent with specific permissions
        config.proxy.agents.push(ProxyAgentConfig {
            id: "test-agent".to_string(),
            name: "Test Agent".to_string(),
            api_key: "test-key".to_string(),
            allowed_models: vec!["deepseek/*".to_string(), "test-alloy".to_string()],
        });
        
        // Agent should have access to allowed models
        assert!(check_model_access(&config, "test-agent", "deepseek-chat").is_ok());
        assert!(check_model_access(&config, "test-agent", "deepseek-reasoner").is_ok());
        assert!(check_model_access(&config, "test-agent", "test-alloy").is_ok());
        
        // Agent should NOT have access to other models
        assert!(check_model_access(&config, "test-agent", "kimi/kimi-for-coding").is_err());
        
        // Other agents should be denied (not in configured list)
        assert!(check_model_access(&config, "other-agent", "deepseek-chat").is_err());
    }

    #[test]
    fn test_check_model_access_empty_allowed_models() {
        let mut config = PolyConfig::default();
        config.proxy.default_policy = ProxyAccessPolicy::AllowConfigured;
        
        // Agent with empty allowed_models list
        config.proxy.agents.push(ProxyAgentConfig {
            id: "empty-agent".to_string(),
            name: "Empty Agent".to_string(),
            api_key: "test-key".to_string(),
            allowed_models: vec![],  // Empty list
        });
        
        // With empty list and AllowConfigured policy, agent should be denied
        assert!(check_model_access(&config, "empty-agent", "deepseek-chat").is_err());
        assert!(check_model_access(&config, "empty-agent", "any-model").is_err());
    }

    #[test]
    fn test_auth_error_messages() {
        // Test that error messages are informative
        let mut config = PolyConfig::default();
        config.proxy.default_policy = ProxyAccessPolicy::DenyAll;
        
        let result = check_model_access(&config, "test-agent", "deepseek-chat");
        assert!(result.is_err());
        
        let err = result.unwrap_err();
        assert!(err.contains("denied") || err.contains("not allowed"));
    }
}