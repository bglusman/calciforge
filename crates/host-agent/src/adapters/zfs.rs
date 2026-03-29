//! ZfsAdapter — wraps the existing ZfsExecutor behind the Adapter trait.
//!
//! Supported operations (first element of `args`):
//!   - `list`     – list datasets/snapshots (read-only)
//!   - `snapshot` – create a snapshot; resource = "dataset", args[1] = snapname
//!   - `destroy`  – destroy a dataset or snapshot (approval required by default)
//!   - `get`      – get a property; resource = "dataset", args[1] = property name
//!   - `rollback` – rollback to snapshot (approval required by default)
//!
//! HostOp mapping:
//! ```json
//! { "kind": "zfs", "resource": "tank/media", "args": ["snapshot", "daily-2024-01-15"] }
//! { "kind": "zfs", "resource": "tank",       "args": ["list"] }
//! { "kind": "zfs", "resource": "tank/media@old", "args": ["destroy"] }
//! ```

use async_trait::async_trait;
use serde_json::json;
use tracing::info;

use crate::adapters::{Adapter, ExecutionResult, HostOp, PolicyDecision};
use crate::auth::ClientIdentity;
use crate::error::AppError;
use crate::zfs::{self, ZfsExecutor, ZfsOp};
use crate::AppState;

pub struct ZfsAdapter {
    executor: ZfsExecutor,
}

impl ZfsAdapter {
    pub fn new() -> Self {
        Self {
            executor: ZfsExecutor::new(),
        }
    }
}

impl Default for ZfsAdapter {
    fn default() -> Self {
        Self::new()
    }
}


#[async_trait]
impl Adapter for ZfsAdapter {
    fn kind(&self) -> &'static str {
        "zfs"
    }

    async fn validate(&self, state: &AppState, op: &HostOp) -> Result<PolicyDecision, AppError> {
        let command = op
            .command()
            .ok_or_else(|| AppError::InvalidDataset("args[0] (command) is required".into()))?;

        // Validate allowed commands
        match command {
            "list" | "snapshot" | "destroy" | "get" | "rollback" => {}
            other => {
                return Ok(PolicyDecision::Deny {
                    reason: format!("ZfsAdapter: unsupported command '{other}'"),
                });
            }
        }

        // Validate resource name when present
        if let Some(ref resource) = op.resource {
            let valid = if resource.contains('@') {
                zfs::is_valid_dataset_or_snapshot(resource)
            } else {
                zfs::is_valid_dataset_name(resource)
            };

            if !valid {
                return Ok(PolicyDecision::Deny {
                    reason: format!("ZfsAdapter: invalid dataset/snapshot name '{resource}'"),
                });
            }
        }

        // Validate snapshot name if provided
        if command == "snapshot" {
            if let Some(snapname) = op.args.get(1) {
                if !zfs::is_valid_snapshot_name(snapname) {
                    return Ok(PolicyDecision::Deny {
                        reason: format!("ZfsAdapter: invalid snapshot name '{snapname}'"),
                    });
                }
            } else {
                return Ok(PolicyDecision::Deny {
                    reason: "ZfsAdapter: snapshot requires args[1] = snapname".into(),
                });
            }
        }

        // Policy check using the config rules
        let config = state.config.get().await;
        let operation_key = format!("zfs-{command}");
        let target = op.resource.as_deref().unwrap_or("");
        let agent_cfg = config.find_agent(
            &state
                .agent_registry
                .resolve_cn_placeholder()
                .unwrap_or_default(),
        );

        if config.requires_approval_for_agent(&operation_key, target, agent_cfg) {
            let token_msg = op
                .approval_token()
                .map(|_| "token provided — will attempt to consume".to_string())
                .unwrap_or_else(|| {
                    format!("send CONFIRM <token> to approve; operation: {operation_key}/{target}")
                });

            return Ok(PolicyDecision::RequiresApproval {
                message: token_msg,
            });
        }

        Ok(PolicyDecision::Allow)
    }

    async fn execute(
        &self,
        _state: &AppState,
        identity: &ClientIdentity,
        op: &HostOp,
    ) -> Result<ExecutionResult, AppError> {
        let command = op.command().unwrap_or("list");
        let resource = op.resource.as_deref().unwrap_or("");

        match command {
            "list" => {
                let list_type = op.args.get(1).map(|s| s.as_str());
                let dataset = op.resource.as_deref();
                let entries = self
                    .executor
                    .list(dataset, list_type, identity)
                    .await
                    .map_err(AppError::Zfs)?;

                let count = entries.len();
                let entries_json = serde_json::to_value(&entries).unwrap_or(json!([]));

                info!(
                    caller = %identity.cn,
                    count = %count,
                    "ZFS list completed"
                );

                Ok(ExecutionResult::ok(format!("{count} entries"))
                    .with_meta("entries", entries_json))
            }

            "snapshot" => {
                let snapname = op.args.get(1).map(|s| s.as_str()).unwrap_or("");
                let snapshot = format!("{resource}@{snapname}");

                let output = self
                    .executor
                    .execute(&snapshot, ZfsOp::Snapshot, identity)
                    .await
                    .map_err(AppError::Zfs)?;

                info!(
                    caller = %identity.cn,
                    snapshot = %snapshot,
                    "ZFS snapshot created"
                );

                Ok(ExecutionResult::ok(output).with_meta("snapshot", json!(snapshot)))
            }

            "destroy" => {
                let target = if resource.is_empty() {
                    return Err(AppError::InvalidDataset(
                        "resource is required for destroy".into(),
                    ));
                } else {
                    resource
                };

                let output = self
                    .executor
                    .execute(target, ZfsOp::Destroy, identity)
                    .await
                    .map_err(AppError::Zfs)?;

                Ok(ExecutionResult::ok(output).with_meta("destroyed", json!(target)))
            }

            "get" => {
                let property = op.args.get(1).map(|s| s.as_str()).unwrap_or("all");
                let value = self
                    .executor
                    .get_property(resource, property)
                    .await
                    .map_err(AppError::Zfs)?;

                Ok(ExecutionResult::ok(value.clone())
                    .with_meta("property", json!(property))
                    .with_meta("value", json!(value)))
            }

            "rollback" => {
                let output = self
                    .executor
                    .execute(resource, ZfsOp::Rollback, identity)
                    .await
                    .map_err(AppError::Zfs)?;

                Ok(ExecutionResult::ok(output))
            }

            other => Err(AppError::Internal(format!(
                "ZfsAdapter: reached execute for unsupported command '{other}'"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zfs::{is_valid_dataset_name, is_valid_dataset_or_snapshot, is_valid_snapshot_name};

    // Validation-only unit tests (no AppState needed)

    #[test]
    fn test_valid_dataset_names() {
        assert!(is_valid_dataset_name("tank"));
        assert!(is_valid_dataset_name("tank/media"));
        assert!(is_valid_dataset_name("rpool/data/home"));
        assert!(is_valid_dataset_name("my-pool_data.01"));
    }

    #[test]
    fn test_invalid_dataset_names() {
        assert!(!is_valid_dataset_name(""));
        assert!(!is_valid_dataset_name("/tank"));
        assert!(!is_valid_dataset_name("tank/"));
        assert!(!is_valid_dataset_name("tank..media"));
        assert!(!is_valid_dataset_name("tank@media"));
        assert!(!is_valid_dataset_name("tank media"));
        assert!(!is_valid_dataset_name("../tank"));
    }

    #[test]
    fn test_valid_snapshot_names() {
        assert!(is_valid_snapshot_name("daily-2024-01-15"));
        assert!(is_valid_snapshot_name("manual_backup"));
        assert!(is_valid_snapshot_name("snap.v1"));
    }

    #[test]
    fn test_invalid_snapshot_names() {
        assert!(!is_valid_snapshot_name("snap@bad"));
        assert!(!is_valid_snapshot_name("snap/bad"));
        assert!(!is_valid_snapshot_name("snap bad"));
        assert!(!is_valid_snapshot_name(""));
    }

    #[test]
    fn test_dataset_or_snapshot() {
        assert!(is_valid_dataset_or_snapshot("tank/media@daily-2024"));
        assert!(is_valid_dataset_or_snapshot("tank/media"));
        assert!(!is_valid_dataset_or_snapshot("tank/media@bad snap"));
        assert!(!is_valid_dataset_or_snapshot("../etc@snap"));
    }

    #[test]
    fn test_command_from_host_op() {
        let op = HostOp {
            kind: "zfs".into(),
            resource: Some("tank/media".into()),
            args: vec!["snapshot".into(), "daily".into()],
            metadata: Default::default(),
        };
        assert_eq!(op.command(), Some("snapshot"));
    }

    #[test]
    fn test_empty_args_command() {
        let op = HostOp {
            kind: "zfs".into(),
            resource: None,
            args: vec![],
            metadata: Default::default(),
        };
        assert_eq!(op.command(), None);
    }
}
