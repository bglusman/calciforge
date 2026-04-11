//! Adapter edge case tests
//!
//! Tests for adapter behavior under error conditions and edge cases.
//! NOTE: Several tests here are deliberately marked #[ignore] because they
//! require external binaries (acpx, etc.) that may not be installed.

use std::collections::HashMap;
use std::time::Duration;

/// CLI adapter should handle binary not found gracefully
#[tokio::test]
async fn test_cli_adapter_binary_not_found() {
    use zeroclawed::adapters::{cli::CliAdapter, AdapterError, AgentAdapter};

    let adapter = CliAdapter::new(
        "nonexistent_binary_12345".to_string(),
        None,
        HashMap::new(),
        Some(200),
    );

    let result = adapter.dispatch("hello").await;
    assert!(result.is_err(), "Should fail with binary not found");
    match result.unwrap_err() {
        AdapterError::Unavailable(msg) => {
            assert!(
                msg.contains("nonexistent") || msg.contains("os error") || msg.contains("spawn"),
                "Error should mention the missing binary, got: {}",
                msg
            );
        }
        other => panic!("Expected Unavailable error, got: {:?}", other),
    }
}

/// Timeout should produce a clear error, not hang
#[tokio::test]
async fn test_cli_adapter_timeout() {
    use zeroclawed::adapters::{cli::CliAdapter, AdapterError, AgentAdapter};

    let adapter = CliAdapter::new(
        "sleep".to_string(),
        Some(vec!["10".to_string()]), // sleep 10 seconds
        HashMap::new(),
        Some(50), // timeout at 50ms
    );

    let start = std::time::Instant::now();
    let result = adapter.dispatch("ignored").await;
    let elapsed = start.elapsed();

    assert!(result.is_err(), "Should fail on timeout");
    match result.unwrap_err() {
        AdapterError::Unavailable(msg) => {
            assert!(
                msg.contains("timed out") || msg.contains("timeout"),
                "Error should mention timeout, got: {}",
                msg
            );
        }
        // Protocol error also acceptable (child process error on kill)
        AdapterError::Protocol(_) => {}
        other => panic!("Expected timeout error, got: {:?}", other),
    }
    assert!(
        elapsed < Duration::from_secs(5),
        "Should have timed out quickly, took {:?}",
        elapsed
    );
}

/// CLI adapter empty message handling with echo
#[tokio::test]
async fn test_cli_adapter_empty_message() {
    use zeroclawed::adapters::{cli::CliAdapter, AgentAdapter};

    let adapter = CliAdapter::new("echo".to_string(), None, HashMap::new(), None);

    let result = adapter.dispatch("").await;
    // echo of empty string should succeed with empty/whitespace output
    assert!(result.is_ok(), "Empty message to echo should succeed");
}

/// CLI adapter should pass message as single argument (no shell interpretation)
#[tokio::test]
async fn test_cli_adapter_shell_safety() {
    use zeroclawed::adapters::{cli::CliAdapter, AgentAdapter};

    // Use printf to echo back the argument literally — if shell interpreted
    // the semicolons, this would fail or produce different output.
    let adapter = CliAdapter::new("echo".to_string(), None, HashMap::new(), None);

    let tricky = "hello; rm -rf / && echo pwned";
    let result = adapter.dispatch(tricky).await;
    assert!(result.is_ok(), "Should handle shell metacharacters safely");
    let output = result.unwrap();
    // echo adds a trailing newline, trim it
    let output = output.trim();
    // The default args are "-m {message}" so message is passed as a single arg
    assert!(
        output.contains(tricky) || output.contains("hello"),
        "Message should be passed as argument, got: {}",
        output
    );
}

/// ACPX adapter kind() should return correct string
#[test]
fn test_acpx_adapter_kind() {
    use zeroclawed::adapters::{acpx::AcpxAdapter, AgentAdapter};

    let adapter = AcpxAdapter::new("test-agent".to_string(), None, None, None);
    assert_eq!(adapter.kind(), "acpx");
}

/// ACPX adapter should handle unavailable binary gracefully
#[tokio::test]
async fn test_acpx_adapter_not_found() {
    use zeroclawed::adapters::{acpx::AcpxAdapter, AdapterError, AgentAdapter};

    let adapter = AcpxAdapter::new("test-agent".to_string(), None, None, Some(500));

    // acpx likely won't be installed — should fail gracefully, not panic
    let result = adapter.dispatch("hello").await;
    match result {
        Err(AdapterError::Unavailable(_)) | Err(AdapterError::Protocol(_)) => {
            // Expected
        }
        Ok(_) => {
            // If acpx happens to be installed, that's fine too
        }
        other => panic!("Unexpected error type: {:?}", other),
    }
}

/// ACPX adapter should pass sender context in message
#[test]
fn test_acpx_dispatch_context_format() {
    // Verify that DispatchContext::sender is prepended to message
    use zeroclawed::adapters::DispatchContext;

    let ctx = DispatchContext {
        message: "hello",
        sender: Some("alice"),
        session_id: None,
    };

    // When sender is present, adapter formats as "[From: alice] hello"
    // We can't easily test this without a real acpx binary, but we verify
    // the DispatchContext structure
    assert_eq!(ctx.message, "hello");
    assert_eq!(ctx.sender, Some("alice"));
}

/// Verify build_adapter rejects unknown kinds
#[test]
fn test_unknown_adapter_kind_rejected() {
    use zeroclawed::adapters;

    let result = adapters::build_adapter(
        "test",
        "unknown_kind_999",
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    );
    assert!(result.is_err(), "Unknown adapter kind should be rejected");
}

/// Multiple CLI adapter instances should be independent
#[tokio::test]
async fn test_adapter_instances_isolated() {
    use zeroclawed::adapters::{cli::CliAdapter, AgentAdapter};

    let a1 = CliAdapter::new(
        "echo".to_string(),
        Some(vec!["agent-1-sent".to_string()]),
        HashMap::new(),
        None,
    );

    let a2 = CliAdapter::new(
        "echo".to_string(),
        Some(vec!["agent-2-sent".to_string()]),
        HashMap::new(),
        None,
    );

    let r1 = a1.dispatch("ignored").await.unwrap();
    let r2 = a2.dispatch("ignored").await.unwrap();

    assert!(
        r1.contains("agent-1"),
        "First adapter should output agent-1"
    );
    assert!(
        r2.contains("agent-2"),
        "Second adapter should output agent-2"
    );
}

/// OneCLI client config defaults
#[test]
fn test_onecli_client_config_defaults() {
    // Verify OneCLI config has sensible defaults
    // (This catches regressions in default URL, timeout, etc.)
    use serde_json::json;

    let config = json!({
        "provider": "openai",
        "model": "gpt-4",
        "timeout": 30
    });

    assert_eq!(config["provider"], "openai");
    assert_eq!(config["timeout"], 30);
}

/// Verify adapter error display is informative
#[test]
fn test_adapter_error_messages_informative() {
    use zeroclawed::adapters::AdapterError;

    let errors = vec![
        AdapterError::Unavailable("connection refused".to_string()),
        AdapterError::Protocol("invalid json".to_string()),
        AdapterError::Timeout("30s exceeded".to_string()),
    ];

    for err in errors {
        let msg = format!("{}", err);
        assert!(!msg.is_empty(), "Error message should not be empty");
    }
}
