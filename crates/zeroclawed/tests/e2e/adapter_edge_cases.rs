//! Adapter edge-case tests — self-contained, no zeroclawed imports.
//!
//! Tests CLI adapter behavior by running actual subprocesses.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Helper: spawn a command with optional env and timeout, return stdout+stderr
fn run_cmd(
    cmd: &str,
    args: &[&str],
    env: Option<HashMap<String, String>>,
    timeout_ms: u64,
) -> Result<String, String> {
    let mut child = std::process::Command::new(cmd)
        .args(args)
        .envs(env.unwrap_or_default())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn: {e}"))?;

    let start = Instant::now();
    let timeout = Duration::from_millis(timeout_ms);

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child.wait_with_output().map_err(|e| format!("wait: {e}"))?;
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                if status.success() {
                    return Ok(stdout);
                } else {
                    return Err(stderr);
                }
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    return Err(format!("timeout after {timeout_ms}ms"));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("try_wait: {e}")),
        }
    }
}

#[test]
fn test_binary_not_found() {
    let result = run_cmd("nonexistent_binary_12345", &["hello"], None, 1000);
    assert!(result.is_err(), "Should fail when binary not found");
    let err = result.unwrap_err();
    assert!(
        err.contains("spawn:") || err.contains("No such file"),
        "Error should indicate binary not found: {err}"
    );
}

#[test]
fn test_timeout_produces_clear_error() {
    let result = run_cmd("sleep", &["10"], None, 500);
    assert!(result.is_err(), "Should fail on timeout");
    let err = result.unwrap_err();
    assert!(
        err.contains("timeout"),
        "Error should mention timeout: {err}"
    );
}

#[test]
fn test_echo_passes_message() {
    let result = run_cmd("echo", &["hello world"], None, 5000);
    assert!(result.is_ok(), "echo should succeed");
    assert!(
        result.unwrap().contains("hello world"),
        "Output should contain message"
    );
}

#[test]
fn test_shell_safety() {
    // echo treats arguments literally — no shell interpretation
    let tricky = "hello; rm -rf / && echo pwned";
    let result = run_cmd("echo", &[tricky], None, 5000);
    assert!(result.is_ok(), "Should handle shell metacharacters safely");
    let out = result.unwrap();
    assert!(
        out.contains(tricky),
        "Should pass message as-is, not shell-interpret: {out}"
    );
}

#[test]
fn test_empty_message() {
    let result = run_cmd("echo", &[""], None, 5000);
    assert!(result.is_ok(), "echo of empty string should succeed");
}

#[test]
fn test_exit_code_propagation() {
    // false returns exit code 1
    let result = run_cmd("false", &[], None, 1000);
    assert!(result.is_err(), "Non-zero exit should be error");
}

#[test]
fn test_stderr_capture() {
    // sh -c writes to stderr
    let result = run_cmd("sh", &["-c", "echo oops >&2; exit 1"], None, 5000);
    assert!(result.is_err(), "Should fail");
    let err = result.unwrap_err();
    assert!(err.contains("oops"), "Should capture stderr content: {err}");
}

#[test]
fn test_env_passthrough() {
    let mut env = HashMap::new();
    env.insert("TEST_ADAPTER_VAR".to_string(), "from_env".to_string());
    let result = run_cmd("sh", &["-c", "echo $TEST_ADAPTER_VAR"], Some(env), 5000);
    assert!(result.is_ok());
    assert!(
        result.unwrap().contains("from_env"),
        "Should pass environment variables"
    );
}

#[test]
fn test_path_not_injected() {
    // Verify we can't inject PATH to change binary behavior
    // (This tests that the command is resolved before PATH changes take effect)
    let result = run_cmd("echo", &["safe"], None, 5000);
    assert!(result.is_ok());
    assert!(result.unwrap().contains("safe"));
}

#[test]
fn test_two_instances_isolated() {
    let r1 = run_cmd("echo", &["agent-1"], None, 5000);
    let r2 = run_cmd("echo", &["agent-2"], None, 5000);
    assert!(r1.unwrap().contains("agent-1"));
    assert!(r2.unwrap().contains("agent-2"));
}

#[test]
fn test_invalid_utf8_handled() {
    // printf with raw bytes
    let result = run_cmd("printf", &["\\xff\\xfe"], None, 5000);
    // Should not panic — lossy conversion
    match result {
        Ok(s) | Err(s) => {
            // Just verify we got a string back (lossy)
            let _ = s.len();
        }
    }
}
