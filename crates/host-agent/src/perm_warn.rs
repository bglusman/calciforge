//! Runtime sudoers permission-warning probe.
//!
//! At health-check time (and on demand via `/admin/warn-permissions`) this
//! module scans `/etc/sudoers` and all files under `/etc/sudoers.d/` for
//! patterns that are considered overly-broad for the clash-agent use-case:
//!
//!   1. **Bare binary without wrapper** — entries granting unrestricted access
//!      to `/sbin/zfs`, `/usr/sbin/pct`, or `/usr/bin/git` (i.e. not scoped
//!      to the wrapper paths).
//!   2. **NOPASSWD: ALL** — any line granting blanket NOPASSWD ALL.
//!   3. **ALL=(ALL) ALL** without any command restriction.
//!
//! When risky patterns are found:
//!   - A WARN is appended to `audit.jsonl`.
//!   - The `/admin/warn-permissions` endpoint returns the findings as JSON.
//!   - The Prometheus metrics endpoint exposes a gauge
//!     `host_agent_sudoers_risky_entries_total`.
//!
//! This module does NOT modify sudoers; it is purely diagnostic.

use crate::audit::{AuditEvent, AuditLogger};
use crate::metrics::Metrics;
use chrono::Utc;
use glob::glob;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::warn;

/// A single risky sudoers entry detected by the probe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskyEntry {
    /// Source file path (e.g. "/etc/sudoers.d/clash-agent")
    pub file: String,
    /// 1-based line number within that file
    pub line: u32,
    /// The raw (trimmed) text of the line
    pub text: String,
    /// Human-readable description of why this is risky
    pub reason: String,
}

/// Result of a full sudoers scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermWarnResult {
    /// ISO-8601 timestamp of when the scan completed
    pub scanned_at: String,
    /// All detected risky entries across all sudoers files
    pub risky_entries: Vec<RiskyEntry>,
    /// Total files scanned
    pub files_scanned: usize,
}

impl PermWarnResult {
    /// Returns true if any risky entries were found.
    #[allow(dead_code)]
    pub fn has_warnings(&self) -> bool {
        !self.risky_entries.is_empty()
    }
}

/// Risky pattern detectors.  Each returns Some(reason) if the line matches.
type Detector = fn(&str) -> Option<String>;

/// Patterns that indicate overly-broad sudoers entries.
const DETECTORS: &[Detector] = &[
    detect_bare_zfs,
    detect_bare_pct,
    detect_bare_git,
    detect_nopasswd_all,
    detect_all_all_all,
];

fn detect_bare_zfs(line: &str) -> Option<String> {
    // Triggers on lines granting /sbin/zfs or /usr/sbin/zfs without scoping
    // to a specific safe sub-command or wrapper path.
    // Safe wrapper paths contain "zfs-destroy-wrapper" and are not flagged.
    if line.contains("zfs-destroy-wrapper") || line.contains("zfs-safe-wrapper") {
        return None;
    }
    // Entries granting the bare zfs binary
    let bare = ["/sbin/zfs", "/usr/sbin/zfs", "/bin/zfs"];
    for b in &bare {
        if line.contains(b) {
            // If it's scoped to only safe subcommands (list, get, snapshot) that's fine
            let safe_subcmds = ["zfs list", "zfs get", "zfs snapshot"];
            let already_scoped = safe_subcmds.iter().any(|s| line.contains(s));
            if !already_scoped {
                return Some(format!(
                    "Broad access to '{}' granted without sub-command restriction; \
                     consider using /usr/local/sbin/zfs-destroy-wrapper for destroy operations",
                    b
                ));
            }
        }
    }
    None
}

fn detect_bare_pct(line: &str) -> Option<String> {
    if line.contains("pct-create-wrapper") {
        return None;
    }
    // Flag unrestricted pct create access
    if line.contains("/usr/sbin/pct create") || line.contains("/usr/bin/pct create") {
        return Some(
            "Direct 'pct create' access granted; use /usr/local/sbin/pct-create-wrapper instead"
                .to_string(),
        );
    }
    // Flag completely unrestricted pct
    let bare = ["/usr/sbin/pct", "/usr/bin/pct"];
    for b in &bare {
        // Allow specific safe sub-commands
        let safe_subcmds = ["pct status", "pct start", "pct stop", "pct list"];
        let is_bare = line.contains(b) && !line.contains("pct create");
        let already_scoped = safe_subcmds.iter().any(|s| line.contains(s));
        if is_bare && !already_scoped && !line.contains("pct-create-wrapper") {
            // Only warn if 'pct' appears as the last token or followed by a wildcard
            if line.ends_with(b) || line.contains(&format!("{} *", b)) {
                return Some(format!(
                    "Blanket access to '{}' granted; scope to specific sub-commands or use wrappers",
                    b
                ));
            }
        }
    }
    None
}

fn detect_bare_git(line: &str) -> Option<String> {
    if line.contains("git-safe-wrapper") {
        return None;
    }
    let bare = ["/usr/bin/git", "/bin/git"];
    for b in &bare {
        if line.contains(b) {
            return Some(format!(
                "Direct access to '{}' granted; use /usr/local/sbin/git-safe-wrapper instead \
                 to restrict to allowlisted repos and safe sub-commands",
                b
            ));
        }
    }
    None
}

fn detect_nopasswd_all(line: &str) -> Option<String> {
    // Matches: user ALL=(ALL) NOPASSWD: ALL  (or NOPASSWD:ALL)
    let upper = line.to_uppercase();
    if upper.contains("NOPASSWD") && upper.ends_with("ALL") {
        // Avoid false-positive on scoped NOPASSWD lines that just happen to end
        // with a path component called "all" — check for a bare ALL token
        if upper.contains("NOPASSWD: ALL") || upper.contains("NOPASSWD:ALL") {
            return Some(
                "NOPASSWD: ALL grants unrestricted root execution without a password; \
                 scope to specific wrapper paths only"
                    .to_string(),
            );
        }
    }
    None
}

fn detect_all_all_all(line: &str) -> Option<String> {
    // Matches: user ALL=(ALL) ALL  or  user ALL=(ALL:ALL) ALL
    let upper = line.to_uppercase();
    // Look for the triple-ALL pattern (excluding NOPASSWD variants which are caught above)
    if (upper.contains("ALL=(ALL") && upper.ends_with(") ALL")
        || (upper.contains("ALL=(ALL)") && upper.ends_with("ALL")))
        && !upper.contains("NOPASSWD")
    {
        return Some(
            "ALL=(ALL) ALL grants unrestricted sudo with a password prompt; \
                 consider scoping to specific wrapper paths"
                .to_string(),
        );
    }
    None
}

/// Scan a single sudoers file and return any risky entries found.
fn scan_file(path: &Path) -> Vec<RiskyEntry> {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(), // unreadable file — skip silently
    };

    let file_str = path.to_string_lossy().to_string();
    let mut findings = Vec::new();

    for (idx, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();

        // Skip comments and empty lines
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        for detector in DETECTORS {
            if let Some(reason) = detector(line) {
                findings.push(RiskyEntry {
                    file: file_str.clone(),
                    line: (idx + 1) as u32,
                    text: line.to_string(),
                    reason,
                });
                break; // one finding per line is enough
            }
        }
    }

    findings
}

/// Scan all sudoers files and return aggregated results.
pub fn scan_sudoers() -> PermWarnResult {
    let mut all_files: Vec<PathBuf> = Vec::new();

    // Primary sudoers file
    let main = PathBuf::from("/etc/sudoers");
    if main.exists() {
        all_files.push(main);
    }

    // Drop-in directory
    if let Ok(entries) = glob("/etc/sudoers.d/*") {
        for entry in entries.flatten() {
            if entry.is_file() {
                all_files.push(entry);
            }
        }
    }

    let files_scanned = all_files.len();
    let mut risky_entries: Vec<RiskyEntry> = Vec::new();

    for path in &all_files {
        risky_entries.extend(scan_file(path));
    }

    PermWarnResult {
        scanned_at: Utc::now().to_rfc3339(),
        risky_entries,
        files_scanned,
    }
}

/// Run the perm-warn probe, update metrics, and write to audit log if warnings found.
///
/// This is called from the `/health` endpoint and `/admin/warn-permissions`.
pub fn probe_and_record(
    audit: &Arc<AuditLogger>,
    metrics: &Arc<Metrics>,
    risky_gauge: &Arc<AtomicU64>,
) -> PermWarnResult {
    let result = scan_sudoers();
    let count = result.risky_entries.len() as u64;

    // Update the gauge
    risky_gauge.store(count, Ordering::Relaxed);
    metrics.record_sudoers_risky(count);

    if count > 0 {
        warn!(
            risky_count = count,
            files_scanned = result.files_scanned,
            "Risky sudoers entries detected — see /admin/warn-permissions"
        );

        // Write to audit log
        let summary = result
            .risky_entries
            .iter()
            .map(|e| format!("{}:{} — {}", e.file, e.line, e.reason))
            .collect::<Vec<_>>()
            .join("; ");

        let _ = audit.log(AuditEvent {
            timestamp: Utc::now(),
            audit_id: uuid::Uuid::new_v4().to_string(),
            caller: "system/perm-warn-probe".to_string(),
            caller_uid: 0,
            operation: "perm-warn-probe".to_string(),
            target: "sudoers".to_string(),
            approval_id: None,
            result: "WARN".to_string(),
            details: Some(summary),
            token_hash: None,
        });
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(line: &str) -> bool {
        DETECTORS.iter().any(|d| d(line).is_some())
    }

    fn reason(line: &str) -> Option<String> {
        DETECTORS.iter().find_map(|d| d(line))
    }

    #[test]
    fn test_bare_zfs_flagged() {
        assert!(check("clash-agent ALL=(root) NOPASSWD: /sbin/zfs"));
        assert!(check("clash-agent ALL=(root) NOPASSWD: /sbin/zfs *"));
        assert!(check(
            "clash-agent ALL=(root) NOPASSWD: /sbin/zfs destroy *"
        ));
    }

    #[test]
    fn test_zfs_safe_subcmds_not_flagged() {
        assert!(!check("clash-agent ALL=(root) NOPASSWD: /sbin/zfs list *"));
        assert!(!check("clash-agent ALL=(root) NOPASSWD: /sbin/zfs get *"));
        assert!(!check(
            "clash-agent ALL=(root) NOPASSWD: /sbin/zfs snapshot *"
        ));
    }

    #[test]
    fn test_zfs_destroy_wrapper_not_flagged() {
        assert!(!check(
            "clash-agent ALL=(root) NOPASSWD: /usr/local/sbin/zfs-destroy-wrapper"
        ));
    }

    #[test]
    fn test_pct_create_flagged() {
        assert!(check(
            "clash-agent ALL=(root) NOPASSWD: /usr/sbin/pct create *"
        ));
    }

    #[test]
    fn test_pct_create_wrapper_not_flagged() {
        assert!(!check(
            "clash-agent ALL=(root) NOPASSWD: /usr/local/sbin/pct-create-wrapper"
        ));
    }

    #[test]
    fn test_bare_git_flagged() {
        assert!(check("clash-agent ALL=(root) NOPASSWD: /usr/bin/git *"));
        assert!(check("clash-agent ALL=(root) NOPASSWD: /usr/bin/git pull"));
    }

    #[test]
    fn test_git_safe_wrapper_not_flagged() {
        assert!(!check(
            "clash-agent ALL=(root) NOPASSWD: /usr/local/sbin/git-safe-wrapper"
        ));
    }

    #[test]
    fn test_nopasswd_all_flagged() {
        assert!(check("root ALL=(ALL) NOPASSWD: ALL"));
        assert!(check("clash-agent ALL=(ALL) NOPASSWD:ALL"));
    }

    #[test]
    fn test_comment_not_flagged() {
        // Comment lines are filtered in scan_file() before detectors run.
        // Verify that scan_file skips them by testing with a temp file.
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "# clash-agent ALL=(root) NOPASSWD: /sbin/zfs").unwrap();
        writeln!(f, "# NOPASSWD: ALL").unwrap();
        let findings = scan_file(f.path());
        assert!(
            findings.is_empty(),
            "Comment lines should not produce findings, got: {:?}",
            findings
        );
    }

    #[test]
    fn test_empty_line_not_flagged() {
        assert!(!check(""));
        assert!(!check("   "));
    }

    #[test]
    fn test_reason_message_is_helpful() {
        let r = reason("clash-agent ALL=(root) NOPASSWD: /usr/bin/git *");
        assert!(r.is_some());
        let msg = r.unwrap();
        assert!(
            msg.contains("git-safe-wrapper"),
            "should mention the wrapper"
        );
    }
}
