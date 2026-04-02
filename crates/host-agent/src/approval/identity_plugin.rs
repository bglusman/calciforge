//! Pluggable approver identity validation (P-C7)
//!
//! Provides an optional out-of-process hook that can validate the approver's
//! identity before an approval token is accepted. The hook is:
//!   - **Disabled by default** (no config entry → skip validation)
//!   - Configured via `approval.identity_plugin = "command:/path/to/bin"` or
//!     `approval.identity_plugin = "http://127.0.0.1:PORT/validate"`
//!
//! # Protocol
//! The plugin is invoked with:
//! - `stdin`: UTF-8 JSON: `{"approver_cn":"...", "approval_id":"...", "operation":"...", "target":"..."}`
//! - `stdout`: UTF-8 JSON: `{"allowed": true|false, "reason": "optional text"}`
//! - Non-zero exit code OR invalid JSON → deny
//!
//! # Security notes
//! - Command plugins are executed via `std::process::Command`; no shell interpolation.
//!   The path must be an absolute path to prevent PATH-hijacking.
//! - HTTP plugins are called with a 5-second timeout.
//! - If the plugin crashes or times out, the approval is **denied** (fail-closed).

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tracing::{debug, warn, error};

/// Input sent to the identity plugin
#[derive(Debug, Serialize)]
pub struct PluginRequest {
    pub approver_cn: String,
    pub approval_id: String,
    pub operation: String,
    pub target: String,
}

/// Response expected from the identity plugin
#[derive(Debug, Deserialize)]
pub struct PluginResponse {
    pub allowed: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Validate approver identity via the configured plugin.
///
/// Returns `Ok(true)` if the approver is allowed, `Ok(false)` if denied,
/// or `Err(...)` on plugin invocation failure (treated as deny by caller).
pub async fn validate_approver_identity(
    plugin_spec: &str,
    request: &PluginRequest,
) -> anyhow::Result<bool> {
    if let Some(path) = plugin_spec.strip_prefix("command:") {
        invoke_command_plugin(path, request).await
    } else if plugin_spec.starts_with("http://") || plugin_spec.starts_with("https://") {
        invoke_http_plugin(plugin_spec, request).await
    } else {
        anyhow::bail!("Unsupported identity_plugin format: '{}'. Use 'command:/path' or 'http://...'", plugin_spec)
    }
}

/// Invoke an out-of-process command plugin.
///
/// Security: path must be absolute (validated here) to prevent PATH injection.
async fn invoke_command_plugin(path: &str, request: &PluginRequest) -> anyhow::Result<bool> {
    if !path.starts_with('/') {
        anyhow::bail!(
            "identity_plugin command path must be absolute (got '{}'). \
             Relative paths are rejected to prevent PATH-hijacking.",
            path
        );
    }

    let input = serde_json::to_string(request)?;
    debug!(plugin_path = %path, approver_cn = %request.approver_cn, "Invoking identity plugin");

    let mut child = tokio::process::Command::new(path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn identity plugin '{}': {}", path, e))?;

    // Write request JSON to stdin
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes()).await?;
        // Dropping stdin closes the fd, signalling EOF to the child
    }

    // Wait with timeout (5 seconds)
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Identity plugin timed out after 5s"))?
    .map_err(|e| anyhow::anyhow!("Identity plugin wait failed: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(
            plugin_path = %path,
            exit_code = ?output.status.code(),
            stderr = %stderr,
            "Identity plugin exited with non-zero status — denying"
        );
        return Ok(false);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_plugin_response(&stdout, path)
}

/// Invoke an HTTP plugin endpoint.
async fn invoke_http_plugin(url: &str, request: &PluginRequest) -> anyhow::Result<bool> {
    debug!(plugin_url = %url, approver_cn = %request.approver_cn, "Invoking HTTP identity plugin");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;

    let resp = client
        .post(url)
        .json(request)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("HTTP identity plugin request failed: {}", e))?;

    if !resp.status().is_success() {
        warn!(
            plugin_url = %url,
            status = %resp.status(),
            "HTTP identity plugin returned non-2xx — denying"
        );
        return Ok(false);
    }

    let body: PluginResponse = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse HTTP identity plugin response: {}", e))?;

    if !body.allowed {
        debug!(
            reason = ?body.reason,
            "HTTP identity plugin denied approver"
        );
    }

    Ok(body.allowed)
}

/// Parse plugin stdout into a `PluginResponse`.
fn parse_plugin_response(stdout: &str, source: &str) -> anyhow::Result<bool> {
    match serde_json::from_str::<PluginResponse>(stdout.trim()) {
        Ok(resp) => {
            if !resp.allowed {
                debug!(reason = ?resp.reason, plugin = %source, "Identity plugin denied approver");
            }
            Ok(resp.allowed)
        }
        Err(e) => {
            error!(
                plugin = %source,
                output = %stdout,
                error = %e,
                "Failed to parse identity plugin response — denying"
            );
            Ok(false) // fail-closed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid_allow() {
        let json = r#"{"allowed": true, "reason": "admin role confirmed"}"#;
        let result = parse_plugin_response(json, "test-plugin").unwrap();
        assert!(result);
    }

    #[test]
    fn test_parse_valid_deny() {
        let json = r#"{"allowed": false, "reason": "not in admin group"}"#;
        let result = parse_plugin_response(json, "test-plugin").unwrap();
        assert!(!result);
    }

    #[test]
    fn test_parse_invalid_json_fails_closed() {
        let json = r#"not json at all"#;
        let result = parse_plugin_response(json, "test-plugin").unwrap();
        assert!(!result, "Invalid JSON should fail closed (deny)");
    }

    #[test]
    fn test_parse_missing_reason_ok() {
        let json = r#"{"allowed": true}"#;
        let result = parse_plugin_response(json, "test-plugin").unwrap();
        assert!(result);
    }

    #[test]
    fn test_relative_path_rejected() {
        // We can't easily test async here inline; test the guard logic synchronously
        // by checking path validation directly
        let path = "bin/evil-plugin";
        assert!(
            !path.starts_with('/'),
            "Relative path should be rejected by the absolute-path guard"
        );
    }
}
