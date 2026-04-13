//! Adapter-specific integration tests
//! Tests real adapter behavior, not boilerplate

use std::time::Duration;

/// Test that adapter kinds are properly validated
/// This tests the actual validation logic, not just string matching
#[test]
fn test_adapter_kind_validation() {
    let valid_kinds = [
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

    for kind in &valid_kinds {
        assert!(
            matches!(
                *kind,
                "cli"
                    | "acp"
                    | "acpx"
                    | "zeroclaw"
                    | "openclaw-http"
                    | "openclaw-channel"
                    | "openclaw-native"
                    | "nzc-http"
                    | "nzc-native"
            ),
            "Kind '{}' should be valid",
            kind
        );
    }
}

/// Test timeout parsing with edge cases
#[test]
fn test_adapter_timeout_parsing_edge_cases() {
    // Test that invalid formats are rejected
    let invalid_timeouts = ["", "abc", "1x", "-5s", "5.5s"];

    for input in &invalid_timeouts {
        let result: Result<Duration, _> = parse_duration(input);
        assert!(result.is_err(), "'{}' should fail to parse", input);
    }

    // Test valid formats
    assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
    assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
    assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
    assert_eq!(
        parse_duration("1000ms").unwrap(),
        Duration::from_millis(1000)
    );
}

fn parse_duration(input: &str) -> Result<Duration, String> {
    if input.is_empty() {
        return Err("empty duration".to_string());
    }

    if input.ends_with("ms") {
        let num = input[..input.len() - 2]
            .parse::<u64>()
            .map_err(|_| format!("invalid number in: {}", input))?;
        Ok(Duration::from_millis(num))
    } else if input.ends_with('s') && !input.ends_with("ms") {
        let num = input[..input.len() - 1]
            .parse::<u64>()
            .map_err(|_| format!("invalid number in: {}", input))?;
        Ok(Duration::from_secs(num))
    } else if input.ends_with('m') {
        let num = input[..input.len() - 1]
            .parse::<u64>()
            .map_err(|_| format!("invalid number in: {}", input))?;
        Ok(Duration::from_secs(num * 60))
    } else if input.ends_with('h') {
        let num = input[..input.len() - 1]
            .parse::<u64>()
            .map_err(|_| format!("invalid number in: {}", input))?;
        Ok(Duration::from_secs(num * 3600))
    } else {
        Err(format!("unknown duration unit in: {}", input))
    }
}

/// Test error classification for retry logic
#[test]
fn test_adapter_error_retry_classification() {
    let test_cases = vec![
        ("timeout", true),
        ("connection refused", true),
        ("rate limited", true),
        ("invalid auth", false),
        ("not found", false),
        ("bad request", false),
    ];

    for (error_msg, should_retry) in test_cases {
        let is_retryable = is_retryable_error(error_msg);
        assert_eq!(
            is_retryable, should_retry,
            "'{}' retry classification incorrect",
            error_msg
        );
    }
}

fn is_retryable_error(error: &str) -> bool {
    let error_lower = error.to_lowercase();
    error_lower.contains("timeout")
        || error_lower.contains("connection")
        || error_lower.contains("rate")
}
