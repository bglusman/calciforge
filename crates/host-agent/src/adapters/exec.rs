//! ExecAdapter — stub for running allowlisted commands or dispatching to Ansible.
//!
//! This adapter is **disabled by default**.  It must be explicitly enabled in config:
//! ```toml
//! [exec]
//! enabled = false  # must set to true to activate
//! allowed_commands = ["/usr/bin/uptime", "/usr/bin/df"]
//! ```
//!
//! Only commands in the `allowed_commands` list may be executed.
//! No shell interpolation: argv is split on whitespace and each token passed separately.
//!
//! Ansible integration is stubbed: if `ansible_job_queue` is set in config, the command
//! is recorded as a job spec in that queue directory.  No actual Ansible execution yet.
//!
//! HostOp mapping:
//! ```json
//! { "kind": "exec", "resource": "/usr/bin/uptime", "args": ["run"] }
//! { "kind": "exec", "resource": "ansible://playbooks/deploy.yml", "args": ["run"] }
//! ```

use async_trait::async_trait;
use std::path::Path;
use tokio::process::Command;
use tracing::{info, warn};

use crate::adapters::{Adapter, ExecutionResult, HostOp, PolicyDecision};
use crate::auth::ClientIdentity;
use crate::error::AppError;
use crate::AppState;

pub struct ExecAdapter;

impl ExecAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ExecAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Adapter for ExecAdapter {
    fn kind(&self) -> &'static str {
        "exec"
    }

    async fn validate(&self, state: &AppState, op: &HostOp) -> Result<PolicyDecision, AppError> {
        let config = state.config.get().await;

        // Adapter must be explicitly enabled
        let exec_cfg = match &config.exec {
            Some(cfg) if cfg.enabled => cfg,
            _ => {
                return Ok(PolicyDecision::Deny {
                    reason: "ExecAdapter is disabled (set exec.enabled = true to enable)".into(),
                });
            }
        };

        let command = op
            .command()
            .ok_or_else(|| AppError::Internal("exec: args[0] (command) is required".into()))?;

        if command != "run" {
            return Ok(PolicyDecision::Deny {
                reason: format!("ExecAdapter: unsupported command '{command}' (only 'run')"),
            });
        }

        let resource = op.resource.as_deref().ok_or_else(|| {
            AppError::Internal("exec: resource (command path) is required".into())
        })?;

        // Ansible stub path
        if resource.starts_with("ansible://") {
            if exec_cfg.ansible_job_queue.is_some() {
                return Ok(PolicyDecision::RequiresApproval {
                    message: format!("Ansible job '{resource}' requires approval before queuing"),
                });
            }
            return Ok(PolicyDecision::Deny {
                reason: "Ansible execution not configured (set exec.ansible_job_queue)".into(),
            });
        }

        // Must be absolute path
        if !Path::new(resource).is_absolute() {
            return Ok(PolicyDecision::Deny {
                reason: format!("ExecAdapter: command path must be absolute, got '{resource}'"),
            });
        }

        // Must be in allowlist
        if !exec_cfg.allowed_commands.iter().any(|c| c == resource) {
            return Ok(PolicyDecision::Deny {
                reason: format!("ExecAdapter: command '{resource}' not in allowed_commands list"),
            });
        }

        Ok(PolicyDecision::Allow)
    }

    async fn execute(
        &self,
        state: &AppState,
        identity: &ClientIdentity,
        op: &HostOp,
    ) -> Result<ExecutionResult, AppError> {
        let config = state.config.get().await;
        let resource = op
            .resource
            .as_deref()
            .ok_or_else(|| AppError::Internal("exec: resource required".into()))?;

        // Ansible stub
        if let Some(playbook) = resource.strip_prefix("ansible://") {
            if let Some(ref exec_cfg) = config.exec {
                if let Some(ref queue_dir) = exec_cfg.ansible_job_queue {
                    // Write a job spec to the queue directory (stub)
                    let job_id = uuid::Uuid::new_v4().to_string();
                    let job_path = format!("{queue_dir}/{job_id}.json");
                    let job_spec = serde_json::json!({
                        "id": job_id,
                        "playbook": playbook,
                        "caller": identity.cn,
                        "requested_at": chrono::Utc::now().to_rfc3339(),
                    });

                    std::fs::write(&job_path, serde_json::to_string_pretty(&job_spec).unwrap())
                        .map_err(|e| {
                            AppError::Internal(format!("Failed to write job spec: {e}"))
                        })?;

                    warn!(
                        caller = %identity.cn,
                        playbook = %playbook,
                        job_id = %job_id,
                        "Ansible job queued (stub)"
                    );

                    return Ok(ExecutionResult::ok(format!("Ansible job queued: {job_id}")));
                }
            }
            return Err(AppError::Internal(
                "Ansible job queue not configured".into(),
            ));
        }

        info!(
            caller = %identity.cn,
            command = %resource,
            "ExecAdapter executing allowlisted command"
        );

        // Extra args after "run"
        let extra_args: Vec<&str> = op.args.iter().skip(1).map(|s| s.as_str()).collect();

        let output = Command::new(resource)
            .args(&extra_args)
            .output()
            .await
            .map_err(|e| AppError::Internal(format!("Failed to spawn command: {e}")))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            Ok(ExecutionResult::ok(stdout))
        } else {
            Err(AppError::Internal(format!(
                "Command '{resource}' failed: {stderr}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    
    use crate::adapters::HostOp;

    #[test]
    fn test_exec_op_structure() {
        let op = HostOp {
            kind: "exec".into(),
            resource: Some("/usr/bin/uptime".into()),
            args: vec!["run".into()],
            metadata: Default::default(),
        };
        assert_eq!(op.command(), Some("run"));
        assert_eq!(op.resource.as_deref(), Some("/usr/bin/uptime"));
    }

    #[test]
    fn test_ansible_stub_detection() {
        let resource = "ansible://playbooks/deploy.yml";
        assert!(resource.starts_with("ansible://"));
        let playbook = &resource["ansible://".len()..];
        assert_eq!(playbook, "playbooks/deploy.yml");
    }

    #[test]
    fn test_absolute_path_check() {
        assert!(std::path::Path::new("/usr/bin/uptime").is_absolute());
        assert!(!std::path::Path::new("uptime").is_absolute());
        assert!(!std::path::Path::new("./uptime").is_absolute());
    }
}
