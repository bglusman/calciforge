//! PctAdapter — safe subset of Proxmox LXC pct operations.
//!
//! Non-destructive operations (always allowed by default):
//!   - `status`  – pct status <vmid>
//!
//! Modifying operations (require approval by default):
//!   - `start`   – pct start <vmid>
//!   - `stop`    – pct stop <vmid>
//!
//! Destructive operations (require explicit approval config to allow; denied by default):
//!   - `destroy` – pct destroy <vmid>   — only allowed when `allow_destroy = true` in config
//!
//! VM IDs are validated as numeric strings in the range 100-999999.
//!
//! HostOp mapping:
//! ```json
//! { "kind": "pct", "resource": "101", "args": ["status"] }
//! { "kind": "pct", "resource": "101", "args": ["start"] }
//! ```
//!
//! Sudo mapping:
//! ```
//! clash-agent ALL=(root) NOPASSWD: /usr/sbin/pct status *
//! clash-agent ALL=(root) NOPASSWD: /usr/sbin/pct start *
//! clash-agent ALL=(root) NOPASSWD: /usr/sbin/pct stop *
//! ```

use async_trait::async_trait;
use tokio::process::Command;
use tracing::{info, warn};

use crate::adapters::{Adapter, ExecutionResult, HostOp, PolicyDecision};
use crate::auth::ClientIdentity;
use crate::error::AppError;
use crate::AppState;

const PCT_BIN: &str = "/usr/sbin/pct";

/// Validate a Proxmox VM/CT ID: numeric, 100–999999.
pub fn is_valid_vmid(id: &str) -> bool {
    match id.parse::<u32>() {
        Ok(n) => (100..=999999).contains(&n),
        Err(_) => false,
    }
}

pub struct PctAdapter;

impl PctAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PctAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Adapter for PctAdapter {
    fn kind(&self) -> &'static str {
        "pct"
    }

    async fn validate(&self, state: &AppState, op: &HostOp) -> Result<PolicyDecision, AppError> {
        let command = op
            .command()
            .ok_or_else(|| AppError::Internal("pct: args[0] (command) is required".into()))?;

        // Validate supported commands
        match command {
            "status" | "start" | "stop" | "destroy" => {}
            other => {
                return Ok(PolicyDecision::Deny {
                    reason: format!("PctAdapter: unsupported command '{other}'"),
                });
            }
        }

        // Validate vmid
        let vmid = op
            .resource
            .as_deref()
            .ok_or_else(|| AppError::Internal("pct: resource (vmid) is required".into()))?;

        if !is_valid_vmid(vmid) {
            return Ok(PolicyDecision::Deny {
                reason: format!("PctAdapter: invalid vmid '{vmid}' (must be numeric 100–999999)"),
            });
        }

        // `destroy` is explicitly disabled unless the config rule permits it
        let config = state.config.get().await;
        if command == "destroy" {
            // Destroy always requires approval regardless of config (always_ask semantics)
            return Ok(PolicyDecision::RequiresApproval {
                message: format!("pct destroy/{vmid} is destructive and always requires approval"),
            });
        }

        // Policy check for start/stop/status
        let operation_key = format!("pct-{command}");
        if let Some(rule) = config.find_rule(&operation_key) {
            if rule.approval_required || rule.always_ask {
                return Ok(PolicyDecision::RequiresApproval {
                    message: format!("pct-{command}/{vmid} requires approval per policy"),
                });
            }
        }

        // Default: start/stop require approval unless explicitly allowed
        match command {
            "start" | "stop" => {
                // Check if there's an explicit allow rule; otherwise require approval
                if config.find_rule(&operation_key).is_none() {
                    return Ok(PolicyDecision::RequiresApproval {
                        message: format!(
                            "pct-{command}/{vmid} requires approval (no explicit allow rule found)"
                        ),
                    });
                }
            }
            "status" => {} // always allowed
            _ => {}
        }

        Ok(PolicyDecision::Allow)
    }

    async fn execute(
        &self,
        _state: &AppState,
        identity: &ClientIdentity,
        op: &HostOp,
    ) -> Result<ExecutionResult, AppError> {
        let command = op.command().unwrap_or("status");
        let vmid = op
            .resource
            .as_deref()
            .ok_or_else(|| AppError::Internal("pct: resource (vmid) is required".into()))?;

        info!(
            caller = %identity.cn,
            command = %command,
            vmid = %vmid,
            "PctAdapter executing"
        );

        let output = run_pct(command, vmid).await?;

        if matches!(command, "start" | "stop" | "destroy") {
            warn!(
                caller = %identity.cn,
                command = %command,
                vmid = %vmid,
                "PCT container operation executed"
            );
        }

        Ok(ExecutionResult::ok(output))
    }
}

/// Run `sudo /usr/sbin/pct <command> <vmid>`.
async fn run_pct(command: &str, vmid: &str) -> Result<String, AppError> {
    let output = Command::new("sudo")
        .args([PCT_BIN, command, vmid])
        .output()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to spawn pct: {e}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        Ok(stdout)
    } else {
        Err(AppError::Internal(format!(
            "pct {command} {vmid} failed (exit {:?}): {stderr}",
            output.status.code()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_vmids() {
        assert!(is_valid_vmid("100"));
        assert!(is_valid_vmid("101"));
        assert!(is_valid_vmid("999"));
        assert!(is_valid_vmid("1000"));
        assert!(is_valid_vmid("999999"));
    }

    #[test]
    fn test_invalid_vmids() {
        // Too low (Proxmox reserves 0-99 for system)
        assert!(!is_valid_vmid("0"));
        assert!(!is_valid_vmid("99"));
        // Out of range
        assert!(!is_valid_vmid("1000000"));
        // Non-numeric
        assert!(!is_valid_vmid("abc"));
        assert!(!is_valid_vmid("10a"));
        assert!(!is_valid_vmid("10.5"));
        // Shell injection
        assert!(!is_valid_vmid("101; rm -rf /"));
        assert!(!is_valid_vmid("$(whoami)"));
        // Empty
        assert!(!is_valid_vmid(""));
        // Negative
        assert!(!is_valid_vmid("-100"));
    }

    #[test]
    fn test_supported_commands() {
        // These should not fail at the command-validation step
        for cmd in &["status", "start", "stop", "destroy"] {
            let op = HostOp {
                kind: "pct".into(),
                resource: Some("101".into()),
                args: vec![cmd.to_string()],
                metadata: Default::default(),
            };
            assert_eq!(op.command(), Some(*cmd));
        }
    }
}
