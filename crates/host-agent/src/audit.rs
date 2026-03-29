//! Structured audit logging with rotation support (P2-14)

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::{debug, error, info, warn};

/// Audit event structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub timestamp: DateTime<Utc>,
    #[serde(rename = "audit_id")]
    pub audit_id: String,
    pub caller: String,
    #[serde(rename = "caller_uid")]
    pub caller_uid: u32,
    pub operation: String,
    pub target: String,
    #[serde(rename = "approval_id")]
    pub approval_id: Option<String>,
    pub result: String,
    pub details: Option<String>,
    /// SHA-256 hash of approval token (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_hash: Option<String>,
}

/// Rotation strategy
#[derive(Debug, Clone, Copy)]
pub enum RotationStrategy {
    Never,
    Hourly,
    Daily,
}

impl From<&str> for RotationStrategy {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "hourly" => RotationStrategy::Hourly,
            "daily" => RotationStrategy::Daily,
            _ => RotationStrategy::Never,
        }
    }
}

/// Audit logger that writes JSONL with rotation
pub struct AuditLogger {
    base_path: PathBuf,
    current_file: Mutex<File>,
    current_date: Mutex<String>, // YYYY-MM-DD format for daily rotation
    rotation: RotationStrategy,
    retention_days: u32,
}

impl AuditLogger {
    /// Create a new audit logger
    pub fn new<P: AsRef<Path>>(
        path: P,
        rotation: RotationStrategy,
        retention_days: u32,
    ) -> Result<Self> {
        let path = path.as_ref();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create audit log directory: {:?}", parent)
            })?;
        }

        let base_path = path.to_path_buf();
        let current_date = Self::current_date_string();
        let current_file_path = Self::rotated_path(&base_path, &current_date);

        // Open file in append mode
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .write(true)
            .open(&current_file_path)
            .with_context(|| format!("Failed to open audit log: {:?}", current_file_path))?;

        info!(
            path = %current_file_path.display(),
            rotation = ?rotation,
            "Audit logger initialized"
        );

        Ok(Self {
            base_path,
            current_file: Mutex::new(file),
            current_date: Mutex::new(current_date),
            rotation,
            retention_days,
        })
    }

    /// Log an audit event
    pub fn log(&self, event: AuditEvent) -> Result<()> {
        // Check if rotation needed
        self.check_rotation()?;

        let json = serde_json::to_string(&event).with_context(|| "Failed to serialize audit event")?;

        let mut file = self.current_file.lock().map_err(|e| {
            anyhow::anyhow!("Failed to lock audit file: {}", e)
        })?;

        writeln!(file, "{}", json).with_context(|| {
            format!("Failed to write to audit log")
        })?;

        file.flush().with_context(|| "Failed to flush audit log")?;

        debug!(
            audit_id = %event.audit_id,
            caller = %event.caller,
            operation = %event.operation,
            result = %event.result,
            "Audit event logged"
        );

        Ok(())
    }

    /// Check if log rotation is needed and perform it
    fn check_rotation(&self) -> Result<()> {
        if matches!(self.rotation, RotationStrategy::Never) {
            return Ok(());
        }

        let new_date = Self::current_date_string();
        let mut current_date = self.current_date.lock().map_err(|e| {
            anyhow::anyhow!("Failed to lock current_date: {}", e)
        })?;

        if new_date != *current_date {
            info!(
                old_date = %*current_date,
                new_date = %new_date,
                "Rotating audit log"
            );

            // Open new file
            let new_path = Self::rotated_path(&self.base_path, &new_date);
            let new_file = OpenOptions::new()
                .create(true)
                .append(true)
                .write(true)
                .open(&new_path)
                .with_context(|| format!("Failed to open new audit log: {:?}", new_path))?;

            // Replace file handle
            let mut file = self.current_file.lock().map_err(|e| {
                anyhow::anyhow!("Failed to lock file: {}", e)
            })?;
            *file = new_file;
            *current_date = new_date;

            // Clean up old logs
            drop(file);
            drop(current_date);
            self.cleanup_old_logs()?;
        }

        Ok(())
    }

    /// Get current date string for rotation
    fn current_date_string() -> String {
        chrono::Local::now().format("%Y-%m-%d").to_string()
    }

    /// Generate rotated log file path
    fn rotated_path(base: &Path, date: &str) -> PathBuf {
        let stem = base.file_stem().unwrap_or_default();
        let ext = base.extension().unwrap_or_default();
        
        let rotated_name = if ext.is_empty() {
            format!("{}.{}", stem.to_string_lossy(), date)
        } else {
            format!("{}.{}.{}", stem.to_string_lossy(), date, ext.to_string_lossy())
        };
        
        base.with_file_name(rotated_name)
    }

    /// Clean up audit logs older than retention period
    fn cleanup_old_logs(&self) -> Result<()> {
        let parent = self.base_path.parent()
            .ok_or_else(|| anyhow::anyhow!("Cannot get parent directory"))?;
        let base_stem = self.base_path.file_stem()
            .ok_or_else(|| anyhow::anyhow!("Cannot get file stem"))?
            .to_string_lossy();

        let cutoff = Utc::now() - chrono::Duration::days(self.retention_days as i64);

        for entry in std::fs::read_dir(parent)? {
            let entry = entry?;
            let path = entry.path();
            
            if let Some(filename) = path.file_stem().and_then(|s| s.to_str()) {
                // Check if this is a rotated log file
                if filename.starts_with(&*base_stem) && filename.contains('.') {
                    // Extract date from filename
                    let parts: Vec<&str> = filename.split('.').collect();
                    if parts.len() >= 2 {
                        if let Ok(date) = chrono::NaiveDate::parse_from_str(parts[1], "%Y-%m-%d") {
                            let datetime = chrono::DateTime::<Utc>::from_utc(
                                date.and_hms_opt(0, 0, 0).unwrap(),
                                Utc,
                            );
                            if datetime < cutoff {
                                info!(path = %path.display(), "Removing old audit log");
                                if let Err(e) = std::fs::remove_file(&path) {
                                    warn!(path = %path.display(), error = %e, "Failed to remove old audit log");
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Get the base log path
    pub fn path(&self) -> &Path {
        &self.base_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_rotated_path() {
        let base = Path::new("/var/log/clash/audit.jsonl");
        let rotated = AuditLogger::rotated_path(base, "2024-01-15");
        assert_eq!(rotated, PathBuf::from("/var/log/clash/audit.2024-01-15.jsonl"));
    }

    #[test]
    fn test_audit_event_serialization() {
        let event = AuditEvent {
            timestamp: Utc::now(),
            audit_id: "test-123".to_string(),
            caller: "librarian".to_string(),
            caller_uid: 1000,
            operation: "zfs-snapshot".to_string(),
            target: "tank@media@daily".to_string(),
            approval_id: None,
            result: "success".to_string(),
            details: Some("Snapshot created".to_string()),
            token_hash: None,
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"audit_id\":\"test-123\""));
        assert!(json.contains("\"caller\":\"librarian\""));
    }

    #[test]
    fn test_rotation_strategy_from_str() {
        assert!(matches!(RotationStrategy::from("daily"), RotationStrategy::Daily));
        assert!(matches!(RotationStrategy::from("DAILY"), RotationStrategy::Daily));
        assert!(matches!(RotationStrategy::from("hourly"), RotationStrategy::Hourly));
        assert!(matches!(RotationStrategy::from("never"), RotationStrategy::Never));
        assert!(matches!(RotationStrategy::from("unknown"), RotationStrategy::Never));
    }
}
