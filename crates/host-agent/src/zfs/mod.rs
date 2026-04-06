//! Async ZFS command execution using tokio::process (P2-10)

use crate::auth::ClientIdentity;
use crate::error::ZfsError;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tracing::{debug, info, warn};

/// ZFS operations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZfsOp {
    Snapshot,
    List,
    Destroy,
    Get,
    Rollback,
    Clone,
}

/// ZFS entry from list output
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZfsEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub used: Option<String>,
    pub available: Option<String>,
    pub refer: Option<String>,
    pub mountpoint: Option<String>,
}

/// ZFS executor that runs commands as specific Unix users
pub struct ZfsExecutor {
    // In the future, this could cache connections or use a pool
}

impl ZfsExecutor {
    /// Create a new ZFS executor
    pub fn new() -> Self {
        Self {}
    }

    /// Execute a ZFS operation
    pub async fn execute(
        &self,
        target: &str,
        op: ZfsOp,
        identity: &ClientIdentity,
    ) -> Result<String, ZfsError> {
        match op {
            ZfsOp::Snapshot => self.snapshot(target, identity).await,
            ZfsOp::Destroy => self.destroy(target, identity).await,
            ZfsOp::Rollback => self.rollback(target, identity).await,
            _ => Err(ZfsError::InvalidOperation(format!("{:?}", op))),
        }
    }

    /// Create a snapshot
    async fn snapshot(
        &self,
        snapshot: &str,
        identity: &ClientIdentity,
    ) -> Result<String, ZfsError> {
        // Execute as the client identity user (P0-3)
        // Uses 'zfs allow' delegation — no sudo needed for snapshot if properly delegated
        let output = run_as_user("zfs", &["snapshot", snapshot], identity).await?;

        info!(
            snapshot = %snapshot,
            caller = %identity.cn,
            uid = %identity.uid,
            "Created ZFS snapshot"
        );

        Ok(output)
    }

    /// Destroy a snapshot or dataset (requires sudo in practice)
    async fn destroy(&self, target: &str, identity: &ClientIdentity) -> Result<String, ZfsError> {
        // Destructive operations require sudo
        // The clash-agent user has sudo rights to destroy via /etc/sudoers
        // Execute as the caller's identity (P0-3)
        let output = run_with_sudo_as_user("zfs", &["destroy", target], identity).await?;

        warn!(
            target = %target,
            caller = %identity.cn,
            uid = %identity.uid,
            "Destroyed ZFS dataset/snapshot"
        );

        Ok(output)
    }

    /// Rollback to a snapshot
    async fn rollback(
        &self,
        snapshot: &str,
        identity: &ClientIdentity,
    ) -> Result<String, ZfsError> {
        let output = run_with_sudo_as_user("zfs", &["rollback", snapshot], identity).await?;

        warn!(
            snapshot = %snapshot,
            caller = %identity.cn,
            uid = %identity.uid,
            "Rolled back ZFS snapshot"
        );

        Ok(output)
    }

    /// List ZFS datasets/snapshots
    pub async fn list(
        &self,
        dataset: Option<&str>,
        list_type: Option<&str>,
        _identity: &ClientIdentity,
    ) -> Result<Vec<ZfsEntry>, ZfsError> {
        let mut args = vec![
            "list",
            "-H",
            "-o",
            "name,type,used,available,refer,mountpoint",
        ];

        // Add type filter if specified
        if let Some(t) = list_type {
            args.push("-t");
            args.push(t);
        }

        // Add dataset if specified
        if let Some(d) = dataset {
            args.push(d);
        }

        let output = run_zfs(&args).await?;
        parse_zfs_list(&output)
    }

    /// Get ZFS property
    pub async fn get_property(&self, dataset: &str, property: &str) -> Result<String, ZfsError> {
        let args = vec!["get", "-H", "-o", "value", property, dataset];
        let output = run_zfs(&args).await?;
        Ok(output.trim().to_string())
    }
}

impl Default for ZfsExecutor {
    fn default() -> Self {
        Self::new()
    }
}

/// Run zfs command directly (for read-only/list operations)
async fn run_zfs(args: &[&str]) -> Result<String, ZfsError> {
    let cmd_str = format!("zfs {}", args.join(" "));
    debug!("Executing: {}", cmd_str);

    let output = Command::new("zfs")
        .args(args)
        .output()
        .await
        .map_err(|e| ZfsError::Execution(format!("Failed to spawn zfs: {}", e)))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(parse_zfs_error(&stderr))
    }
}

/// Run command as a specific Unix user using sudo -u (P0-3, P2-10)
async fn run_as_user(
    cmd: &str,
    args: &[&str],
    identity: &ClientIdentity,
) -> Result<String, ZfsError> {
    let username = &identity.username;

    debug!(
        cmd = %cmd,
        args = ?args,
        user = %username,
        uid = %identity.uid,
        "Executing command as user"
    );

    // Use sudo to run as the target user
    // The clash-agent user should have NOPASSWD sudo to run zfs as specified users
    let output = Command::new("sudo")
        .args([&["-u", username, cmd], args].concat())
        .output()
        .await
        .map_err(|e| ZfsError::Execution(format!("Failed to spawn sudo: {}", e)))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(parse_zfs_error(&stderr))
    }
}

/// Run command with sudo as a specific user (for privileged operations)
async fn run_with_sudo_as_user(
    cmd: &str,
    args: &[&str],
    identity: &ClientIdentity,
) -> Result<String, ZfsError> {
    debug!(
        cmd = %cmd,
        args = ?args,
        user = %identity.username,
        uid = %identity.uid,
        "Executing command with sudo as user"
    );

    // Run sudo -u <user> <cmd> to execute as the specific user
    // This requires: clash-agent ALL=(librarian,lucien) NOPASSWD: /sbin/zfs
    let output = Command::new("sudo")
        .args([&["-u", &identity.username, cmd], args].concat())
        .output()
        .await
        .map_err(|e| ZfsError::Execution(format!("Failed to spawn sudo: {}", e)))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(parse_zfs_error(&stderr))
    }
}

/// Parse ZFS error messages into typed errors
fn parse_zfs_error(stderr: &str) -> ZfsError {
    let stderr_lower = stderr.to_lowercase();

    if stderr_lower.contains("permission denied") || stderr_lower.contains("cannot open") {
        ZfsError::PermissionDenied(stderr.to_string())
    } else if stderr_lower.contains("no such") || stderr_lower.contains("does not exist") {
        ZfsError::DatasetNotFound(stderr.to_string())
    } else if stderr_lower.contains("cannot destroy") {
        ZfsError::InvalidOperation(stderr.to_string())
    } else if stderr_lower.contains("dataset is busy") {
        ZfsError::DatasetBusy(stderr.to_string())
    } else {
        ZfsError::Execution(stderr.to_string())
    }
}

/// Parse ZFS list output
fn parse_zfs_list(output: &str) -> Result<Vec<ZfsEntry>, ZfsError> {
    let mut entries = Vec::new();

    for line in output.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 6 {
            entries.push(ZfsEntry {
                name: parts[0].to_string(),
                kind: parts[1].to_string(),
                used: Some(parts[2].to_string()),
                available: Some(parts[3].to_string()),
                refer: Some(parts[4].to_string()),
                mountpoint: Some(parts[5].to_string()),
            });
        }
    }

    Ok(entries)
}

/// Validate a ZFS dataset name
pub fn is_valid_dataset_name(name: &str) -> bool {
    // Basic validation: no spaces, no @ (that's for snapshots), limited special chars
    let re = Regex::new(r"^[a-zA-Z0-9_][a-zA-Z0-9_\-/.]*$").unwrap();
    re.is_match(name) && !name.contains("..") && !name.starts_with('/') && !name.ends_with('/')
}

/// Validate a snapshot name
pub fn is_valid_snapshot_name(name: &str) -> bool {
    // Similar to dataset but more restrictive
    let re = Regex::new(r"^[a-zA-Z0-9_][a-zA-Z0-9_\-.]*$").unwrap();
    re.is_match(name) && !name.contains('@') && !name.contains('/')
}

/// Validate a full dataset@snapshot format
pub fn is_valid_dataset_or_snapshot(name: &str) -> bool {
    if let Some(idx) = name.find('@') {
        // It's a snapshot reference
        let dataset = &name[..idx];
        let snap = &name[idx + 1..];
        is_valid_dataset_name(dataset) && is_valid_snapshot_name(snap)
    } else {
        // It's a dataset
        is_valid_dataset_name(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_dataset_name() {
        assert!(is_valid_dataset_name("tank"));
        assert!(is_valid_dataset_name("tank/media"));
        assert!(is_valid_dataset_name("tank/media/photos"));
        assert!(is_valid_dataset_name("my-pool_data"));
    }

    #[test]
    fn test_invalid_dataset_name() {
        assert!(!is_valid_dataset_name(""));
        assert!(!is_valid_dataset_name("/tank"));
        assert!(!is_valid_dataset_name("tank/"));
        assert!(!is_valid_dataset_name("tank..media"));
        assert!(!is_valid_dataset_name("tank@media"));
        assert!(!is_valid_dataset_name("tank media"));
    }

    #[test]
    fn test_valid_snapshot_name() {
        assert!(is_valid_snapshot_name("daily-2024-01-15"));
        assert!(is_valid_snapshot_name("manual_backup"));
        // '@' is NOT valid in a snapshot name component — it's used as the separator
        // between dataset and snapshot in "dataset@snapshot" references
        assert!(
            !is_valid_snapshot_name("snap@123"),
            "@ should be invalid in snapshot name"
        );
        assert!(is_valid_snapshot_name("snap123"));
        assert!(is_valid_snapshot_name("snap.v1"));
    }

    #[test]
    fn test_parse_zfs_list() {
        let output = "tank\tfilesystem\t10G\t100G\t10G\t/mnt/tank\n\
                      tank@media@daily\tsnapshot\t1G\t-\t1G\t-\n";
        let entries = parse_zfs_list(output).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "tank");
        assert_eq!(entries[0].kind, "filesystem");
        assert_eq!(entries[1].name, "tank@media@daily");
        assert_eq!(entries[1].kind, "snapshot");
    }
}
