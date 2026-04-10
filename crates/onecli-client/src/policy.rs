//! Policy enforcement integration with clash

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyCheck {
    pub tool: String,
    pub args: serde_json::Value,
    pub context: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyResult {
    pub allowed: bool,
    pub reason: Option<String>,
    pub action: PolicyAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAction {
    Allow,
    Deny,
    Ask,
    Redact,
}

/// Check a tool against policy via clashd (not local clash binary)
/// 
/// DEPRECATED: This module is being replaced by clashd sidecar + zeroclawed-policy-plugin.
/// For new code, use the clashd HTTP API directly or the plugin hook.
/// 
/// This function now FAILS CLOSED (denies on error) for security.
pub async fn check_tool(policy_file: Option<&std::path::Path>, check: &PolicyCheck) -> PolicyResult {
    let _ = policy_file; // Unused - kept for API compatibility
    let _ = check; // Unused - kept for API compatibility
    
    // This module is deprecated. clashd is the new policy enforcement path.
    // Fail closed rather than allowing potentially unsafe operations.
    warn!("onecli-client policy module is deprecated, use clashd instead");
    PolicyResult {
        allowed: false,
        reason: Some("Policy module deprecated, use clashd".to_string()),
        action: PolicyAction::Deny,
    }
}

async fn check_with_clash(policy_file: &std::path::Path, check: &PolicyCheck) -> anyhow::Result<PolicyResult> {
    let check_json = serde_json::to_string(check)?;
    
    let output = tokio::process::Command::new("clash")
        .args(&["check", "--policy", policy_file.to_str().unwrap()])
        .arg(&check_json)
        .output()
        .await?;
    
    if output.status.success() {
        let result: PolicyResult = serde_json::from_slice(&output.stdout)?;
        Ok(result)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Clash check failed: {}", stderr)
    }
}
