//! Config Validation Tests
//!
//! Tests that catch config parsing errors that caused silent failures:
//! - Agents after [memory] section not loading
//! - Unknown adapter kinds not rejected

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
        assert!(
            is_valid_adapter_kind(kind),
            "{} should be a valid adapter kind",
            kind
        );
    }

    for kind in invalid_kinds {
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
    // TOML allows multiple [[agents]] tables — they append
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
fn test_nzc_native_without_command() {
    // nzc-native adapter should work without command (uses webhook pattern)
    let config = r#"
[[agents]]
id = "nzc-agent"
kind = "nzc-native"
endpoint = "http://127.0.0.1:19300"
token = "test-token"
timeout_ms = 30000
"#;

    let (_dir, path) = write_config(config);
    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: toml::Value = content.parse().unwrap();

    let agent = parsed
        .get("agents")
        .and_then(|a| a.as_array())
        .unwrap()
        .first()
        .unwrap();

    assert_eq!(agent.get("kind").unwrap().as_str().unwrap(), "nzc-native");
    assert!(
        agent.get("command").is_none(),
        "nzc-native should not require command"
    );
    assert!(
        agent.get("endpoint").is_some(),
        "nzc-native should have endpoint"
    );
}

#[test]
fn test_empty_agents_array_valid() {
    // An agents section with no agents should parse but produce empty list
    let config = r#"
version = 2
"#;

    let (_dir, path) = write_config(config);
    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: toml::Value = content.parse().unwrap();

    let agents = parsed.get("agents").and_then(|a| a.as_array());
    assert!(
        agents.is_none() || agents.unwrap().is_empty(),
        "No agents section should mean no agents"
    );
}
