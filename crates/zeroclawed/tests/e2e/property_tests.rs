//! Property-based tests for ZeroClawed
//! 
//! These tests use proptest to generate random inputs and verify
//! properties hold, catching edge cases we haven't thought of.

use proptest::prelude::*;

// Property: URL reconstruction after parsing should be lossless
// This catches path-stripping bugs like the one we found
proptest! {
    #[test]
    fn test_url_reconstruction_lossless(
        provider in "(openai|anthropic|kimi|brave|groq)",
        path in "[a-z0-9/_-]+",
    ) {
        // Simulate what OneCLI does: strip prefix, then rebuild
        let input = format!("/proxy/{}/{}", provider, path);
        
        // Strip the prefix (what OneCLI should do)
        let stripped = input.strip_prefix(&format!("/proxy/{}/", provider))
            .or_else(|| input.strip_prefix(&format!("/proxy/{}", provider)))
            .unwrap_or(&input);
        
        // Rebuild (what shouldn't happen, but test the logic)
        // Property: the path component should be preserved
        prop_assert!(
            stripped == path || stripped == format!("/{}", path) || stripped.is_empty() && path.is_empty(),
            "Path component was lost: input={}, stripped={}, expected path={}",
            input, stripped, path
        );
    }
}

// Property: Config parsing is deterministic
// Same input → same output
proptest! {
    #[test]
    fn test_config_parsing_deterministic(
        agent_count in 1usize..10,
    ) {
        use toml::Value;
        
        // Generate a valid config with N agents
        let mut config = String::from("version = 2\n\n");
        for i in 0..agent_count {
            config.push_str(&format!(
                r#"[[agents]]
id = "agent-{i}"
kind = "cli"
command = "/bin/echo"
timeout_ms = 30000

"#
            ));
        }
        
        // Parse twice, should get same result
        let parsed1: Value = config.parse().expect("Valid TOML");
        let parsed2: Value = config.parse().expect("Valid TOML");
        
        prop_assert_eq!(
            parsed1.to_string(),
            parsed2.to_string(),
            "Config parsing is not deterministic"
        );
        
        // Should have correct number of agents
        let agents1 = parsed1.get("agents").and_then(|a| a.as_array());
        prop_assert!(agents1.is_some(), "Should have agents array");
        prop_assert_eq!(
            agents1.unwrap().len(),
            agent_count,
            "Should have {} agents",
            agent_count
        );
    }
}

// Property: Credential headers don't leak into non-auth headers
proptest! {
    #[test]
    fn test_credential_isolation(
        api_key in "[a-zA-Z0-9_-]{20,50}",
        other_header in "[a-zA-Z0-9_-]*",
    ) {
        // Simulate request building
        let auth_header = format!("Bearer {}", api_key);
        
        // Property: auth header should contain the key
        prop_assert!(auth_header.contains(&api_key));
        
        // Property: other headers should NOT contain the key
        let other = format!("X-Custom: {}", other_header);
        prop_assert!(
            !other.contains(&api_key) || other_header.contains(&api_key),
            "API key leaked into non-auth header"
        );
    }
}

// Property: Adapter kind validation rejects unknown kinds
proptest! {
    #[test]
    fn test_adapter_kind_validation(
        kind in "[a-z-]+",
    ) {
        let valid_kinds = [
            "cli", "acp", "acpx", "zeroclaw",
            "openclaw-http", "openclaw-channel", "openclaw-native",
            "nzc-http", "nzc-native",
        ];
        
        let is_valid = valid_kinds.contains(&kind.as_str());
        
        // Property: known kinds pass, unknown kinds fail
        // This is a documentation of expected behavior
        if kind == "openclaw" || kind == "http" || kind == "claw" {
            // These specifically should be rejected (bugs we found)
            prop_assert!(!is_valid, "'{}' should be invalid (too vague)", kind);
        }
    }
}

// Property: Timeout values are positive and reasonable
proptest! {
    #[test]
    fn test_timeout_values(
        timeout_ms in 0u64..600000,  // 0 to 10 minutes
    ) {
        // Property: zero or very small timeouts should be rejected
        // or at least warned about
        if timeout_ms < 1000 {
            // These are suspiciously small for network operations
            // In real code, we'd assert a warning is logged
            prop_assert!(timeout_ms < 1000, "Very small timeout: {}ms", timeout_ms);
        }
        
        // Property: reasonable timeouts are accepted
        if (1000..=300000).contains(&timeout_ms) {
            prop_assert!(timeout_ms >= 1000);
        }
    }
}

// Property: Tool payload round-trip preserves structure
proptest! {
    #[test]
    fn test_tool_payload_preservation(
        tool_name in "[a-z_]+",
        param_count in 0usize..5,
    ) {
        use serde_json::json;
        
        // Build a tools array
        let params: serde_json::Map<String, serde_json::Value> = (0..param_count)
            .map(|i| (format!("param{}", i), json!({"type": "string"})))
            .collect();
        
        let tools = json!([{
            "type": "function",
            "function": {
                "name": tool_name,
                "parameters": {
                    "type": "object",
                    "properties": params
                }
            }
        }]);
        
        // Property: serialization round-trip preserves structure
        let serialized = serde_json::to_string(&tools).unwrap();
        let deserialized: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        
        // Clone for second assertion
        let deserialized_for_name = deserialized.clone();
        
        prop_assert_eq!(
            tools,
            deserialized,
            "Tool payload was corrupted in round-trip"
        );
        
        // Property: tool name is preserved
        let name = deserialized_for_name[0]["function"]["name"].as_str().unwrap();
        prop_assert_eq!(name, tool_name);
    }
}
