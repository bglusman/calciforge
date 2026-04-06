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

pub async fn check_tool(policy_file: Option<&std::path::Path>, check: &PolicyCheck) -> PolicyResult {
    let policy_file = match policy_file {
        Some(p) => p,
        None => {
            debug!("No policy file configured, allowing all");
            return PolicyResult {
                allowed: true,
                reason: None,
                action: PolicyAction::Allow,
            };
        }
    };
    
    match check_with_clash(policy_file, check).await {
        Ok(result) => result,
        Err(e) => {
            warn!("Policy check failed: {}, defaulting to allow", e);
            PolicyResult {
                allowed: true,
                reason: Some(format!("Policy check error: {}", e)),
                action: PolicyAction::Allow,
            }
        }
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
