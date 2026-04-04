//! Audit logging: append JSONL events to `~/.nonzeroclawed/logs/outpost-audit.jsonl`.

use crate::verdict::{OutpostVerdict, ScanContext};
use chrono::Utc;
use serde::Serialize;
use std::path::PathBuf;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tracing::warn;

/// A single audit log entry.
#[derive(Debug, Serialize)]
pub struct AuditEntry {
    pub ts: String,
    pub claw_id: String,
    pub tool: String,
    pub url: String,
    pub verdict: String,
    pub reason: Option<String>,
    pub cached: bool,
}

impl AuditEntry {
    pub fn new(claw_id: &str, ctx: ScanContext, url: &str, verdict: &OutpostVerdict, cached: bool) -> Self {
        Self {
            ts: Utc::now().to_rfc3339(),
            claw_id: claw_id.to_string(),
            tool: ctx.as_str().to_string(),
            url: url.to_string(),
            verdict: verdict.name().to_string(),
            reason: verdict.reason().map(|s| s.to_string()),
            cached,
        }
    }
}

/// Async audit logger that appends JSONL to `~/.nonzeroclawed/logs/outpost-audit.jsonl`.
pub struct AuditLogger {
    log_path: PathBuf,
    claw_id: String,
}

impl AuditLogger {
    /// Create a logger with the given claw ID and default log path.
    pub fn new(claw_id: impl Into<String>) -> Self {
        let home = home::home_dir().unwrap_or_else(|| PathBuf::from("/root"));
        Self {
            log_path: home.join(".nonzeroclawed/logs/outpost-audit.jsonl"),
            claw_id: claw_id.into(),
        }
    }

    /// Log a scan event.
    pub async fn log(&self, ctx: ScanContext, url: &str, verdict: &OutpostVerdict, cached: bool) {
        let entry = AuditEntry::new(&self.claw_id, ctx, url, verdict, cached);
        let line = match serde_json::to_string(&entry) {
            Ok(l) => l + "\n",
            Err(e) => {
                warn!("outpost audit serialize error: {e}");
                return;
            }
        };
        if let Some(parent) = self.log_path.parent() {
            if let Err(e) = fs::create_dir_all(parent).await {
                warn!("outpost audit mkdir error: {e}");
                return;
            }
        }
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .await
        {
            Ok(mut f) => {
                if let Err(e) = f.write_all(line.as_bytes()).await {
                    warn!("outpost audit write error: {e}");
                }
            }
            Err(e) => warn!("outpost audit open error: {e}"),
        }
    }
}
