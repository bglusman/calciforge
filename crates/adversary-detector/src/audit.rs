//! Audit logging: append JSONL events to `~/.zeroclawed/logs/adversary-audit.jsonl`.

use crate::verdict::{ScanContext, ScanVerdict};
use chrono::Utc;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
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
    pub fn new(
        claw_id: &str,
        ctx: ScanContext,
        url: &str,
        verdict: &ScanVerdict,
        cached: bool,
    ) -> Self {
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

/// Async audit logger that appends JSONL to `~/.zeroclawed/logs/adversary-audit.jsonl`.
pub struct AuditLogger {
    log_path: PathBuf,
    claw_id: String,
    total_requests: AtomicU64,
    blocked_or_reviewed: AtomicU64,
}

impl AuditLogger {
    /// Create a logger with the given claw ID and default log path.
    pub fn new(claw_id: impl Into<String>) -> Self {
        let home = home::home_dir().unwrap_or_else(|| PathBuf::from("/root"));
        Self {
            log_path: home.join(".zeroclawed/logs/adversary-audit.jsonl"),
            claw_id: claw_id.into(),
            total_requests: AtomicU64::new(0),
            blocked_or_reviewed: AtomicU64::new(0),
        }
    }

    /// Log a scan event.
    /// Total number of scan requests.
    pub fn count(&self) -> u64 {
        self.total_requests.load(Ordering::Relaxed)
    }

    /// Number of requests that returned Blocked or Review verdicts.
    pub fn blocked_and_reviewed(&self) -> u64 {
        self.blocked_or_reviewed.load(Ordering::Relaxed)
    }

    pub async fn log(&self, ctx: ScanContext, url: &str, verdict: &ScanVerdict, cached: bool) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        match verdict {
            ScanVerdict::Unsafe { .. } | ScanVerdict::Review { .. } => {
                self.blocked_or_reviewed.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
        let entry = AuditEntry::new(&self.claw_id, ctx, url, verdict, cached);
        let line = match serde_json::to_string(&entry) {
            Ok(l) => l + "\n",
            Err(e) => {
                warn!("adversary audit serialize error: {e}");
                return;
            }
        };
        if let Some(parent) = self.log_path.parent() {
            if let Err(e) = fs::create_dir_all(parent).await {
                warn!("adversary audit mkdir error: {e}");
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
                    warn!("adversary audit write error: {e}");
                }
            }
            Err(e) => warn!("adversary audit open error: {e}"),
        }
    }
}
