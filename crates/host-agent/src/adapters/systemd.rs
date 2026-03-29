//! SystemdAdapter — safe systemctl delegation.
//!
//! Supported operations (args[0]):
//!   - `status`  – systemctl status <service>   (read-only)
//!   - `start`   – systemctl start <service>
//!   - `stop`    – systemctl stop <service>
//!   - `restart` – systemctl restart <service>
//!
//! Service names are validated against a strict safe regex:
//!   `^[a-zA-Z0-9_][a-zA-Z0-9_\-.@]+\.service$`
//! This rejects path traversal, shell metacharacters, etc.
//!
//! HostOp mapping:
//! ```json
//! { "kind": "systemd", "resource": "nginx.service", "args": ["status"] }
//! { "kind": "systemd", "resource": "myapp.service", "args": ["restart"] }
//! ```
//!
//! Sudo mapping (add to /etc/sudoers.d/host-agent):
//! ```
//! clash-agent ALL=(root) NOPASSWD: /usr/bin/systemctl status *.service
//! clash-agent ALL=(root) NOPASSWD: /usr/bin/systemctl start *.service
//! clash-agent ALL=(root) NOPASSWD: /usr/bin/systemctl stop *.service
//! clash-agent ALL=(root) NOPASSWD: /usr/bin/systemctl restart *.service
//! ```

use async_trait::async_trait;
use regex::Regex;
use std::sync::OnceLock;
use tokio::process::Command;
use tracing::{info, warn};

use crate::adapters::{Adapter, ExecutionResult, HostOp, PolicyDecision};
use crate::auth::ClientIdentity;
use crate::error::AppError;
use crate::AppState;

/// Regex for safe systemd service unit names.
/// Allows: alphanumeric, underscore, hyphen, dot, @ — must end with a known suffix.
static SERVICE_NAME_RE: OnceLock<Regex> = OnceLock::new();
fn service_name_re() -> &'static Regex {
    SERVICE_NAME_RE.get_or_init(|| {
        Regex::new(r"^[a-zA-Z0-9_][a-zA-Z0-9_\-.@]*\.(service|socket|timer|target|mount|path)$")
            .expect("SERVICE_NAME_RE is valid")
    })
}

const SYSTEMCTL_BIN: &str = "/usr/bin/systemctl";

/// Validate a systemd unit name.
pub fn is_valid_service_name(name: &str) -> bool {
    !name.is_empty() && service_name_re().is_match(name)
}

pub struct SystemdAdapter;

impl SystemdAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SystemdAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Adapter for SystemdAdapter {
    fn kind(&self) -> &'static str {
        "systemd"
    }

    async fn validate(&self, state: &AppState, op: &HostOp) -> Result<PolicyDecision, AppError> {
        let command = op
            .command()
            .ok_or_else(|| AppError::Internal("systemd: args[0] (command) is required".into()))?;

        // Validate command
        match command {
            "status" | "start" | "stop" | "restart" => {}
            other => {
                return Ok(PolicyDecision::Deny {
                    reason: format!("SystemdAdapter: unsupported command '{other}'"),
                });
            }
        }

        // Validate service name
        let service = op.resource.as_deref().ok_or_else(|| {
            AppError::Internal("systemd: resource (service name) is required".into())
        })?;

        if !is_valid_service_name(service) {
            return Ok(PolicyDecision::Deny {
                reason: format!(
                    "SystemdAdapter: invalid service name '{service}' (must match safe regex)"
                ),
            });
        }

        // Policy check
        let config = state.config.get().await;
        let operation_key = format!("systemd-{command}");

        // Check against configured rules for systemd operations
        if let Some(rule) = config.find_rule(&operation_key) {
            if rule.approval_required {
                if rule.always_ask {
                    return Ok(PolicyDecision::RequiresApproval {
                        message: format!(
                            "systemd-{command}/{service} always requires approval (always_ask=true)"
                        ),
                    });
                }
                // Pattern check
                if let Some(ref pattern) = rule.pattern {
                    if let Ok(re) = Regex::new(pattern) {
                        if re.is_match(service) {
                            return Ok(PolicyDecision::RequiresApproval {
                                message: format!(
                                    "systemd-{command}/{service} matches approval pattern"
                                ),
                            });
                        }
                    }
                }
            }
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
        let service = op
            .resource
            .as_deref()
            .ok_or_else(|| AppError::Internal("systemd: resource is required".into()))?;

        info!(
            caller = %identity.cn,
            command = %command,
            service = %service,
            "SystemdAdapter executing"
        );

        let output = run_systemctl(command, service).await?;

        if command != "status" {
            warn!(
                caller = %identity.cn,
                command = %command,
                service = %service,
                "Systemd service operation executed"
            );
        }

        Ok(ExecutionResult::ok(output))
    }
}

/// Run `sudo /usr/bin/systemctl <command> <service>`.
async fn run_systemctl(command: &str, service: &str) -> Result<String, AppError> {
    let output = Command::new("sudo")
        .args([SYSTEMCTL_BIN, command, service])
        .output()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to spawn systemctl: {e}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        Ok(stdout)
    } else {
        // `systemctl status` exits non-zero for stopped/inactive — return output anyway
        if command == "status" {
            // Combine stdout + stderr so caller can see the full status
            Ok(format!("{stdout}{stderr}"))
        } else {
            Err(AppError::Internal(format!(
                "systemctl {command} {service} failed: {stderr}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_service_names() {
        assert!(is_valid_service_name("nginx.service"));
        assert!(is_valid_service_name("my-app.service"));
        assert!(is_valid_service_name("my_app.service"));
        assert!(is_valid_service_name("multi-user.target"));
        assert!(is_valid_service_name("sshd.service"));
        assert!(is_valid_service_name("docker.service"));
        assert!(is_valid_service_name("system@1.service"));
        assert!(is_valid_service_name("myapp.timer"));
        assert!(is_valid_service_name("myapp.socket"));
    }

    #[test]
    fn test_invalid_service_names() {
        // No suffix
        assert!(!is_valid_service_name("nginx"));
        // Spaces / shell metacharacters
        assert!(!is_valid_service_name("nginx .service"));
        assert!(!is_valid_service_name("nginx;id.service"));
        assert!(!is_valid_service_name("../etc/passwd.service"));
        // Path traversal
        assert!(!is_valid_service_name("/etc/nginx.service"));
        // Empty
        assert!(!is_valid_service_name(""));
        // Starts with dot
        assert!(!is_valid_service_name(".hidden.service"));
        // Shell injection
        assert!(!is_valid_service_name("$(id).service"));
        assert!(!is_valid_service_name("`whoami`.service"));
        // Unknown suffix
        assert!(!is_valid_service_name("myapp.conf"));
    }

    #[test]
    fn test_command_extraction() {
        let op = HostOp {
            kind: "systemd".into(),
            resource: Some("nginx.service".into()),
            args: vec!["restart".into()],
            metadata: Default::default(),
        };
        assert_eq!(op.command(), Some("restart"));
    }
}
