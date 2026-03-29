//! Adapter-first architecture for host-agent operations.
//!
//! Each capability (ZFS, systemd, pct, git, exec) is implemented as an [`Adapter`].
//! The [`AdapterRegistry`] maps operation kind strings to adapters and drives the
//! unified `/host/op` dispatch path.
//!
//! # Dispatch flow
//! ```text
//! POST /host/op  →  HostOp  →  AdapterRegistry  →  Adapter::validate  →  policy  →  Adapter::execute
//! ```

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::auth::ClientIdentity;
use crate::error::AppError;
use crate::AppState;

pub mod exec;
pub mod git;
pub mod pct;
pub mod registry;
pub mod systemd;
pub mod zfs;

pub use registry::AdapterRegistry;

// ── Core request/response types ────────────────────────────────────────────────

/// Unified operation request dispatched to an adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostOp {
    /// Operation kind — maps to an adapter: "zfs", "systemd", "pct", "git", "exec"
    pub kind: String,

    /// Primary resource identifier.
    /// - zfs: dataset or snapshot name
    /// - systemd: service unit name (e.g. "nginx.service")
    /// - pct: VM/container ID (numeric string)
    /// - git: repository path
    /// - exec: command name
    pub resource: Option<String>,

    /// Operation-specific arguments (e.g. ["snapshot", "mysnap"] or ["status"]).
    /// First element is treated as the sub-command / operation name.
    #[serde(default)]
    pub args: Vec<String>,

    /// Arbitrary metadata (approval tokens, extra flags, etc.)
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl HostOp {
    /// Convenience: first element of args (the sub-command).
    pub fn command(&self) -> Option<&str> {
        self.args.first().map(|s| s.as_str())
    }

    /// Extract an optional approval token from metadata["approval_token"].
    pub fn approval_token(&self) -> Option<&str> {
        self.metadata
            .get("approval_token")
            .and_then(|v| v.as_str())
    }
}

// ── Policy decision ────────────────────────────────────────────────────────────

/// Result of adapter-level policy validation.
#[derive(Debug, Clone)]
pub enum PolicyDecision {
    /// Proceed immediately.
    Allow,
    /// Operation is conditionally allowed; human approval is required.
    RequiresApproval { message: String },
    /// Operation is forbidden by policy.
    Deny { reason: String },
}

// ── Execution result ───────────────────────────────────────────────────────────

/// Result returned by a successful `Adapter::execute`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    /// Human-readable output (stdout, structured data, etc.)
    pub output: String,
    /// Exit code from underlying process (0 = success).
    pub exit_code: i32,
    /// Extra structured data (entries list, service status, etc.)
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl ExecutionResult {
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            exit_code: 0,
            metadata: HashMap::new(),
        }
    }

    pub fn with_meta(mut self, key: &str, val: serde_json::Value) -> Self {
        self.metadata.insert(key.to_string(), val);
        self
    }
}

// ── Adapter trait ─────────────────────────────────────────────────────────────

#[async_trait]
pub trait Adapter: Send + Sync {
    /// Short identifier matching the `HostOp::kind` field.
    fn kind(&self) -> &'static str;

    /// Validate that the operation is well-formed and satisfies adapter-level rules.
    ///
    /// This is called *before* the global policy engine; it should reject syntactically
    /// invalid inputs (bad names, forbidden sub-commands) and signal policy requirements.
    async fn validate(&self, state: &AppState, op: &HostOp) -> Result<PolicyDecision, AppError>;

    /// Execute the operation.  Called only after policy approval is confirmed.
    async fn execute(
        &self,
        state: &AppState,
        identity: &ClientIdentity,
        op: &HostOp,
    ) -> Result<ExecutionResult, AppError>;
}
