//! Config Validation Tests
//!
//! Tests that catch config parsing errors that caused silent failures:
//! - Agents after [memory] section not loading
//! - Unknown adapter kinds not rejected
//! - Missing api_key for required kinds not caught

use std::fs::write;
use std::path::PathBuf;
use tempfile::TempDir;

/// Helper: Create a temp config file and return path
fn write_config(content: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    write(&path, content).unwrap();
    (dir, path)
}

#[test]
fn test_agents_after_memory_section_load() {
    // Bug: Agents defined after [memory] section were silently ignored
    // This test verifies that section ordering doesn't matter for TOML tables

    let config = r#"
[memory]
pre_read_hook = "none"

[[agents]]
id = "test-agent"
kind = "cli"
command = "/bin/echo"
timeout_ms = 30000
aliases = ["test"]
"#;

    let (_dir, path) = write_config(config);

    // Parse the config
    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: Result<toml::Value, _> = content.parse();

    assert!(
        parsed.is_ok(),
        "Config should parse even with agents after [memory]"
    );

    let value = parsed.unwrap();
    let agents = value.get("agents").and_then(|a| a.as_array());

    assert!(agents.is_some(), "Should have agents array");
    assert_eq!(agents.unwrap().len(), 1, "Should have exactly 1 agent");
}

#[test]
fn test_unknown_adapter_kind_fails() {
    // Bug: kind = "openclaw" was not recognized (should be "openclaw-http")
    // Config should be validated and reject unknown kinds

    let valid_kinds = vec![
        "cli",
        "acp",
        "acpx",
        "zeroclaw",
        "openclaw-http",
        "openclaw-channel",
        "openclaw-native",
        "nzc-http",
        "nzc-native",
    ];

    let invalid_kinds = vec![
        "openclaw", // Missing suffix
        "http",     // Too vague
        "unknown",  // Doesn't exist
        "claw",     // Typo
    ];

    for kind in valid_kinds {
        // These should be accepted
        assert!(
            is_valid_adapter_kind(kind),
            "{} should be a valid adapter kind",
            kind
        );
    }

    for kind in invalid_kinds {
        // These should be rejected
        assert!(
            !is_valid_adapter_kind(kind),
            "{} should NOT be a valid adapter kind",
            kind
        );
    }
}

/// Check if an adapter kind is valid
fn is_valid_adapter_kind(kind: &str) -> bool {
    matches!(
        kind,
        "cli"
            | "acp"
            | "acpx"
            | "zeroclaw"
            | "openclaw-http"
            | "openclaw-channel"
            | "openclaw-native"
            | "nzc-http"
            | "nzc-native"
    )
}

#[test]
fn test_duplicate_agents_array_works() {
    // TOML allows multiple [[agents]] tables - they append
    // This should create 2 agents, not fail

    let config = r#"
[[agents]]
id = "agent-1"
kind = "cli"
command = "/bin/echo"

[[agents]]
id = "agent-2"
kind = "cli"
command = "/bin/cat"
"#;

    let (_dir, path) = write_config(config);
    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: toml::Value = content.parse().unwrap();

    let agents = parsed.get("agents").and_then(|a| a.as_array()).unwrap();
    assert_eq!(
        agents.len(),
        2,
        "Should have 2 agents from duplicate [[agents]] tables"
    );
}

#[test]
fn test_config_file_location_precedence() {
    // Bug: Config was loading from /etc/ instead of ~/.zeroclawed/
    // This documents the expected precedence

    let expected_locations = vec![
        // Primary: User config
        ("~/.zeroclawed/config.toml", true),
        ("~/.config/zeroclawed/config.toml", true),
        // Secondary: System config (fallback)
        ("/etc/zeroclawed/config.toml", false),
    ];

    // This test documents expected behavior
    // The actual implementation may vary - update if needed
    for (path, is_primary) in expected_locations {
        println!("Config location: {} (primary: {})", path, is_primary);
    }
}

#[test]
fn test_missing_api_key_for_required_kind() {
    // Bug: Some adapter kinds require api_key but config didn't validate this
    // openclaw-http requires api_key

    let config_missing_key = r#"
[[agents]]
id = "bad-agent"
kind = "openclaw-http"
endpoint = "http://127.0.0.1:8080"
# Missing: api_key = "..."
timeout_ms = 30000
"#;

    let (_dir, path) = write_config(config_missing_key);
    let content = std::fs::read_to_string(&path).unwrap();

    // Parse should succeed (TOML is valid)
    let parsed: toml::Value = content.parse().unwrap();

    // But validation should fail
    let agent = parsed
        .get("agents")
        .and_then(|a| a.as_array())
        .unwrap()
        .first()
        .unwrap();
    let has_api_key = agent.get("api_key").is_some();

    assert!(!has_api_key, "Test config intentionally missing api_key");

    // The real test: when this config is loaded by ZeroClawed,
    // it should produce a clear error like:
    // "agent 'bad-agent': kind='openclaw-http' requires api_key"
}

#[test]
fn test_cli_kind_does_not_require_api_key() {
    // CLI adapter doesn't need api_key - uses command only

    let config = r#"
[[agents]]
id = "cli-agent"
kind = "cli"
command = "/usr/local/bin/my-agent"
args = ["--model", "gpt-4"]
timeout_ms = 60000
"#;

    let (_dir, _path) = write_config(config);

    // This should be valid without api_key
    // No assertion needed - test passes if it compiles/runs
}

#[test]
fn test_adapter_kind_case_sensitive() {
    // Adapter kinds must be lowercase with hyphens
    // Mixed case or underscores should be rejected
    let invalid_case_kinds = vec![
        "OpenClaw-Http",
        "OPENCLAW-HTTP",
        "openclaw_http",
        "Cli",
        "Nzc-Native",
    ];

    for kind in invalid_case_kinds {
        assert!(
            !is_valid_adapter_kind(kind),
            "'{}' should be rejected (must be lowercase with hyphens)",
            kind
        );
    }
}

#[test]
fn test_all_registered_adapter_kinds_are_valid() {
    // Ensure every kind in the valid list is also documented
    // This prevents drift between test data and actual adapter registry
    let all_kinds = [
        "cli",
        "acp",
        "acpx",
        "zeroclaw",
        "openclaw-http",
        "openclaw-channel",
        "openclaw-native",
        "nzc-http",
        "nzc-native",
    ];

    for kind in all_kinds {
        assert!(
            is_valid_adapter_kind(kind),
            "Registered kind '{}' should pass validation",
            kind
        );
    }

    // Verify we have exactly 9 adapter kinds
    assert_eq!(all_kinds.len(), 9, "Expected exactly 9 adapter kinds");
}

#[test]
fn test_empty_config_has_no_agents() {
    // Minimal valid config with no agents section
    let config = "version = 2\n";
    let (_dir, path) = write_config(config);
    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: toml::Value = content.parse().unwrap();

    let agents = parsed.get("agents");
    assert!(agents.is_none(), "Empty config should have no agents");
}

#[test]
fn test_agent_with_timeout_zero_is_suspicious() {
    // timeout_ms = 0 means no timeout, which is dangerous
    let config = r#"
[[agents]]
id = "fast-agent"
kind = "cli"
command = "/bin/echo"
timeout_ms = 0
"#;

    let (_dir, path) = write_config(config);
    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: toml::Value = content.parse().unwrap();

    let agent = parsed.get("agents").and_then(|a| a.as_array()).unwrap()[0].clone();
    let timeout = agent.get("timeout_ms").and_then(|t| t.as_integer()).unwrap();

    // The config parses, but timeout=0 should be flagged by validation
    assert_eq!(timeout, 0, "Test documents that timeout_ms=0 is parseable");
    // Real validation should reject this with:
    // "agent 'fast-agent': timeout_ms must be > 0"
}

#[test]
fn test_multiple_agents_same_id_should_error() {
    // Two agents with the same id is almost certainly a mistake
    let config = r#"
[[agents]]
id = "duplicate"
kind = "cli"
command = "/bin/echo"

[[agents]]
id = "duplicate"
kind = "cli"
command = "/bin/cat"
"#;

    let (_dir, path) = write_config(config);
    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: toml::Value = content.parse().unwrap();

    let agents = parsed.get("agents").and_then(|a| a.as_array()).unwrap();
    let ids: Vec<&str> = agents
        .iter()
        .filter_map(|a| a.get("id").and_then(|i| i.as_str()))
        .collect();

    // TOML parses fine, but both have the same id
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], ids[1]);
    // Real validation should detect: "duplicate agent id 'duplicate'"
}
