use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Single audit log entry for a proxied request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub timestamp: DateTime<Utc>,
    pub method: String,
    pub url: String,
    pub target_host: String,
    pub outbound_verdict: String,
    pub outbound_findings: Vec<String>,
    pub outbound_scan_ms: u64,
    pub inbound_verdict: Option<String>,
    pub inbound_findings: Option<Vec<String>>,
    pub inbound_scan_ms: Option<u64>,
    pub credentials_injected: Vec<String>,
    pub response_status: Option<u16>,
    pub total_time_ms: u64,
}

/// Thread-safe audit log writer.
pub struct AuditLogger {
    entries: std::sync::Mutex<Vec<AuditEntry>>,
}

impl Default for AuditLogger {
    fn default() -> Self {
        Self::new()
    }
}

impl AuditLogger {
    pub fn new() -> Self {
        Self {
            entries: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn log(&self, entry: AuditEntry) {
        let mut entries = self.entries.lock().unwrap();
        entries.push(entry);
    }

    /// Get recent entries (last N).
    pub fn recent(&self, n: usize) -> Vec<AuditEntry> {
        let entries = self.entries.lock().unwrap();
        let start = if entries.len() > n {
            entries.len() - n
        } else {
            0
        };
        entries[start..].to_vec()
    }

    /// Get entries where verdict was not Allow.
    pub fn blocked_and_reviewed(&self) -> Vec<AuditEntry> {
        let entries = self.entries.lock().unwrap();
        entries
            .iter()
            .filter(|e| {
                e.outbound_verdict != "allow" || e.inbound_verdict.as_deref() != Some("allow")
            })
            .cloned()
            .collect()
    }

    /// Get count of all entries.
    pub fn count(&self) -> usize {
        let entries = self.entries.lock().unwrap();
        entries.len()
    }
}
