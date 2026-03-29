//! Prometheus metrics endpoint (P2-13)

use axum::extract::State;
use axum::response::IntoResponse;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Metrics collector for host-agent
pub struct Metrics {
    /// Total number of requests by endpoint
    requests_total: AtomicU64,
    /// Total number of ZFS operations
    zfs_operations_total: AtomicU64,
    /// ZFS operations by type (snapshot, destroy, list)
    zfs_snapshots_total: AtomicU64,
    zfs_destroys_total: AtomicU64,
    zfs_lists_total: AtomicU64,
    /// Number of approval requests created
    approvals_created_total: AtomicU64,
    /// Number of approvals granted
    approvals_granted_total: AtomicU64,
    /// Number of failed authentication attempts
    auth_failures_total: AtomicU64,
    /// Number of policy denials
    policy_denials_total: AtomicU64,
    /// Number of requests rejected by rate limiter (P-B5)
    rate_limited_total: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            requests_total: AtomicU64::new(0),
            zfs_operations_total: AtomicU64::new(0),
            zfs_snapshots_total: AtomicU64::new(0),
            zfs_destroys_total: AtomicU64::new(0),
            zfs_lists_total: AtomicU64::new(0),
            approvals_created_total: AtomicU64::new(0),
            approvals_granted_total: AtomicU64::new(0),
            auth_failures_total: AtomicU64::new(0),
            policy_denials_total: AtomicU64::new(0),
            rate_limited_total: AtomicU64::new(0),
        }
    }

    pub fn increment_requests(&self) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_zfs_operation(&self, op_type: &str) {
        self.zfs_operations_total.fetch_add(1, Ordering::Relaxed);
        match op_type {
            "snapshot" => self.zfs_snapshots_total.fetch_add(1, Ordering::Relaxed),
            "destroy" => self.zfs_destroys_total.fetch_add(1, Ordering::Relaxed),
            "list" => self.zfs_lists_total.fetch_add(1, Ordering::Relaxed),
            _ => 0,
        };
    }

    pub fn increment_approvals_created(&self) {
        self.approvals_created_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_approvals_granted(&self) {
        self.approvals_granted_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_auth_failures(&self) {
        self.auth_failures_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_policy_denials(&self) {
        self.policy_denials_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_rate_limited(&self) {
        self.rate_limited_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Generate Prometheus-formatted metrics
    pub fn render(&self) -> String {
        format!(
            r#"# HELP host_agent_requests_total Total number of HTTP requests
# TYPE host_agent_requests_total counter
host_agent_requests_total {}

# HELP host_agent_zfs_operations_total Total number of ZFS operations
# TYPE host_agent_zfs_operations_total counter
host_agent_zfs_operations_total {}

# HELP host_agent_zfs_snapshots_total Total number of ZFS snapshot operations
# TYPE host_agent_zfs_snapshots_total counter
host_agent_zfs_snapshots_total {}

# HELP host_agent_zfs_destroys_total Total number of ZFS destroy operations
# TYPE host_agent_zfs_destroys_total counter
host_agent_zfs_destroys_total {}

# HELP host_agent_zfs_lists_total Total number of ZFS list operations
# TYPE host_agent_zfs_lists_total counter
host_agent_zfs_lists_total {}

# HELP host_agent_approvals_created_total Total number of approval requests created
# TYPE host_agent_approvals_created_total counter
host_agent_approvals_created_total {}

# HELP host_agent_approvals_granted_total Total number of approvals granted
# TYPE host_agent_approvals_granted_total counter
host_agent_approvals_granted_total {}

# HELP host_agent_auth_failures_total Total number of authentication failures
# TYPE host_agent_auth_failures_total counter
host_agent_auth_failures_total {}

# HELP host_agent_policy_denials_total Total number of policy denials
# TYPE host_agent_policy_denials_total counter
host_agent_policy_denials_total {}

# HELP host_agent_rate_limited_total Total number of requests rejected by rate limiter
# TYPE host_agent_rate_limited_total counter
host_agent_rate_limited_total {}
"#,
            self.requests_total.load(Ordering::Relaxed),
            self.zfs_operations_total.load(Ordering::Relaxed),
            self.zfs_snapshots_total.load(Ordering::Relaxed),
            self.zfs_destroys_total.load(Ordering::Relaxed),
            self.zfs_lists_total.load(Ordering::Relaxed),
            self.approvals_created_total.load(Ordering::Relaxed),
            self.approvals_granted_total.load(Ordering::Relaxed),
            self.auth_failures_total.load(Ordering::Relaxed),
            self.policy_denials_total.load(Ordering::Relaxed),
            self.rate_limited_total.load(Ordering::Relaxed),
        )
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Metrics endpoint handler
pub async fn metrics_handler(State(state): State<crate::AppState>) -> impl IntoResponse {
    (
        [("Content-Type", "text/plain; version=0.0.4")],
        state.metrics.render(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_rendering() {
        let metrics = Metrics::new();
        metrics.increment_requests();
        metrics.increment_zfs_operation("snapshot");
        metrics.increment_approvals_created();

        let output = metrics.render();
        
        assert!(output.contains("host_agent_requests_total 1"));
        assert!(output.contains("host_agent_zfs_snapshots_total 1"));
        assert!(output.contains("host_agent_approvals_created_total 1"));
    }

    #[test]
    fn test_zfs_operation_types() {
        let metrics = Metrics::new();
        
        metrics.increment_zfs_operation("snapshot");
        metrics.increment_zfs_operation("destroy");
        metrics.increment_zfs_operation("list");
        
        assert_eq!(metrics.zfs_snapshots_total.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.zfs_destroys_total.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.zfs_lists_total.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.zfs_operations_total.load(Ordering::Relaxed), 3);
    }
}
