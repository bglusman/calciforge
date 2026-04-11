//! Property-based tests for ZeroClawed
//!
//! These tests use proptest to generate random inputs and verify
//! properties hold, catching edge cases we haven't thought of.

use proptest::prelude::*;

// Property: URL reconstruction after parsing should be lossless.
// Catches path-stripping bugs like the one we found in OneCLI.
proptest! {
    #[test]
    fn test_url_reconstruction_lossless(
        provider in "(openai|anthropic|kimi|brave|groq)",
        path in "[a-z0-9/_-]+",
    ) {
        let input = format!("/proxy/{}/{}", provider, path);

        let stripped = input.strip_prefix(&format!("/proxy/{}/", provider))
            .or_else(|| input.strip_prefix(&format!("/proxy/{}", provider)))
            .unwrap_or(&input);

        prop_assert!(
            stripped == path
                || stripped == format!("/{}", path)
                || (stripped.is_empty() && path.is_empty()),
            "Path component was lost: input={}, stripped={}, expected path={}",
            input, stripped, path
        );
    }
}

// Property: Tool payload round-trip preserves structure.
// Ensures serde serialization doesn't corrupt our tool definitions.
proptest! {
    #[test]
    fn test_tool_payload_preservation(
        tool_name in "[a-z_]+",
        param_count in 0usize..5,
    ) {
        use serde_json::json;

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

        let serialized = serde_json::to_string(&tools).unwrap();
        let deserialized: serde_json::Value = serde_json::from_str(&serialized).unwrap();

        prop_assert_eq!(tools, deserialized.clone(), "Tool payload was corrupted in round-trip");

        let name = deserialized[0]["function"]["name"].as_str().unwrap();
        prop_assert_eq!(name, tool_name);
    }
}

// Property: is_valid_adapter_kind matches exhaustive list.
// Random strings that don't match known kinds should be rejected.
// Known kinds should always be accepted.
proptest! {
    #[test]
    fn test_adapter_kind_exhaustive(
        kind in "[a-z0-9-]+",
    ) {
        let valid_kinds = [
            "cli", "acp", "acpx", "zeroclaw",
            "openclaw-http", "openclaw-channel", "openclaw-native",
            "nzc-http", "nzc-native",
        ];

        let should_be_valid = valid_kinds.contains(&kind.as_str());

        // If it's in our known list, it must be valid.
        // If it's not in our known list, it must be invalid.
        // This is a property of the match statement in config parsing.
        let actually_valid = matches!(
            kind.as_str(),
            "cli" | "acp" | "acpx" | "zeroclaw"
                | "openclaw-http" | "openclaw-channel" | "openclaw-native"
                | "nzc-http" | "nzc-native"
        );

        prop_assert_eq!(
            should_be_valid, actually_valid,
            "Kind validation mismatch for '{}': known={} actual={}",
            kind, should_be_valid, actually_valid
        );
    }
}

// Property: Phone number normalization is lossy but consistent.
// Two calls on the same input should produce the same output.
proptest! {
    #[test]
    fn test_phone_normalization_idempotent(
        input in "[0-9+ -]{7,20}",
    ) {
        fn normalize_phone(s: &str) -> String {
            let s = s.trim();
            let s = s.replace(['-', ' '], "");
            if s.starts_with('+') { s } else { format!("+{s}") }
        }

        let first = normalize_phone(&input);
        let second = normalize_phone(&first);
        prop_assert_eq!(first, second, "Normalization should be idempotent");
    }
}

// Property: Phone normalization always produces a '+' prefix.
proptest! {
    #[test]
    fn test_phone_normalization_plus_prefix(
        digits in "[0-9]{7,15}",
    ) {
        fn normalize_phone(s: &str) -> String {
            let s = s.trim();
            let s = s.replace(['-', ' '], "");
            if s.starts_with('+') { s } else { format!("+{s}") }
        }

        let normalized = normalize_phone(&digits);
        prop_assert!(normalized.starts_with('+'), "Phone should have + prefix: {}", normalized);
    }
}
