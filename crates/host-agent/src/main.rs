//! ZeroClawed Host-Agent
//!
//! mTLS RPC server providing safe VM-to-host delegation for ZFS, systemd, and PCT.
//!
//! # Security Model
//! - Unix permissions are the enforcement layer (zfs allow, sudo)
//! - Host-agent is a thin RPC wrapper that validates mTLS and logs
//! - Destructive operations (destroy, rollback) require Signal approval
//! - All operations logged to /var/log/clash/audit.jsonl

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::{Extension, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::service::TowerToHyperService;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tower::Service;
use tracing::{error, info, warn};

// Install rustls crypto provider early
use rustls::crypto::ring::default_provider;
use rustls::crypto::CryptoProvider;

mod adapters;
mod approval;
mod audit;
mod auth;
mod config;
mod error;
mod metrics;
mod perm_warn;
mod rate_limit;
mod tls;
mod zfs;

use approval::{ApprovalManager, ApprovalRequest, signal::SignalWebhookPayload};
use approval::identity_plugin::{validate_approver_identity, PluginRequest};
use audit::{AuditEvent, AuditLogger};
use auth::{AgentRegistry, ClientIdentity};
use audit::RotationStrategy;
use config::{Config, ReloadableConfig};
use error::AppError;
use metrics::Metrics;
use rate_limit::{RateLimiter, rate_limit_response};
use tls::IdentityExtractingAcceptor;
use zfs::{ZfsEntry, ZfsExecutor, ZfsOp};
use adapters::{AdapterRegistry, HostOp, PolicyDecision};

/// ZeroClawed Host-Agent CLI
#[derive(Parser, Debug)]
#[command(name = "clash-host-agent")]
#[command(about = "ZeroClawed Host-Agent — mTLS RPC server for host delegation")]
struct Cli {
    /// Path to configuration file
    #[arg(short, long, default_value = "/etc/clash/host-agent.toml")]
    config: PathBuf,

    /// Enable debug logging
    #[arg(short, long)]
    debug: bool,
}

/// Application state shared across handlers
#[derive(Clone)]
pub struct AppState {
    config: ReloadableConfig,
    audit: Arc<AuditLogger>,
    approvals: Arc<ApprovalManager>,
    zfs: Arc<ZfsExecutor>,
    metrics: Arc<Metrics>,
    agent_registry: Arc<AgentRegistry>,
    rate_limiter: Arc<RateLimiter>,
    /// Adapter registry for the unified /host/op dispatch
    adapter_registry: Arc<AdapterRegistry>,
}

// API Request/Response Types

#[derive(Debug, Deserialize)]
struct SnapshotRequest {
    dataset: String,
    snapname: String,
}

#[derive(Debug, Serialize)]
struct SnapshotResponse {
    success: bool,
    snapshot: String,
    audit_id: String,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DestroyRequest {
    dataset: String,
    approval_token: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum DestroyResponse {
    Pending {
        pending_approval: bool,
        approval_id: String,
        message: String,
    },
    Success {
        success: bool,
        audit_id: String,
        message: String,
    },
}

#[derive(Debug, Deserialize)]
struct ListRequest {
    dataset: Option<String>,
    #[serde(rename = "type")]
    list_type: Option<String>, // "snapshot", "filesystem", "volume", "all"
}

#[derive(Debug, Serialize)]
struct ListResponse {
    success: bool,
    entries: Vec<ZfsEntry>,
    audit_id: String,
}

#[derive(Debug, Deserialize)]
struct ApproveRequest {
    approval_id: String,
    token: String,
}

#[derive(Debug, Serialize)]
struct ApproveResponse {
    success: bool,
    message: String,
}

#[derive(Debug, Deserialize)]
struct SignalWebhookBody {
    token: String,
    confirmation_code: String,
    approver: String,
    timestamp: String,
}

// Health check endpoint (no auth required)
async fn health_check(State(state): State<AppState>) -> impl IntoResponse {
    state.metrics.increment_requests();

    // Run perm-warn probe and expose the count in the health response.
    // Scan is lightweight (reads /etc/sudoers* — typically a few KB).
    use std::sync::atomic::AtomicU64;
    let risky_gauge = Arc::new(AtomicU64::new(0));
    let perm_result = perm_warn::probe_and_record(&state.audit, &state.metrics, &risky_gauge);
    let risky_count = perm_result.risky_entries.len();

    Json(serde_json::json!({
        "status": "healthy",
        "version": env!("CARGO_PKG_VERSION"),
        "sudoers_warnings": risky_count,
    }))
}

// ZFS snapshot endpoint (no approval required for delegation)
async fn zfs_snapshot(
    State(state): State<AppState>,
    Extension(identity): Extension<ClientIdentity>,
    Json(req): Json<SnapshotRequest>,
) -> Result<Json<SnapshotResponse>, AppError> {
    state.metrics.increment_requests();
    state.metrics.increment_zfs_operation("snapshot");

    let audit_id = uuid::Uuid::new_v4().to_string();

    // Validate dataset name (basic sanitization)
    if !zfs::is_valid_dataset_name(&req.dataset) {
        return Err(AppError::InvalidDataset(req.dataset));
    }
    if !zfs::is_valid_snapshot_name(&req.snapname) {
        return Err(AppError::InvalidSnapshotName(req.snapname));
    }

    let snapshot = format!("{}@{}", req.dataset, req.snapname);

    // Check if approval required
    let config = state.config.get().await;
    let agent_cfg = config.find_agent(&identity.cn).cloned();
    if config.requires_approval_for_agent("zfs-snapshot", &snapshot, agent_cfg.as_ref()) {
        return Err(AppError::PolicyDenied(
            "Snapshot requires approval per policy".to_string()
        ));
    }

    // Log operation attempt
    state.audit.log(AuditEvent {
        timestamp: chrono::Utc::now(),
        audit_id: audit_id.clone(),
        caller: identity.cn.clone(),
        caller_uid: identity.uid,
        operation: "zfs-snapshot".to_string(),
        target: snapshot.clone(),
        approval_id: None,
        result: "attempting".to_string(),
        details: None,
        token_hash: None,
    })?;

    // Execute snapshot
    let result = state
        .zfs
        .execute(&snapshot, ZfsOp::Snapshot, &identity)
        .await;

    match result {
        Ok(output) => {
            state.audit.log(AuditEvent {
                timestamp: chrono::Utc::now(),
                audit_id: audit_id.clone(),
                caller: identity.cn.clone(),
                caller_uid: identity.uid,
                operation: "zfs-snapshot".to_string(),
                target: snapshot.clone(),
                approval_id: None,
                result: "success".to_string(),
                details: Some(output),
                token_hash: None,
            })?;

            Ok(Json(SnapshotResponse {
                success: true,
                snapshot,
                audit_id,
                message: None,
            }))
        }
        Err(e) => {
            state.audit.log(AuditEvent {
                timestamp: chrono::Utc::now(),
                audit_id: audit_id.clone(),
                caller: identity.cn.clone(),
                caller_uid: identity.uid,
                operation: "zfs-snapshot".to_string(),
                target: snapshot.clone(),
                approval_id: None,
                result: "failure".to_string(),
                details: Some(e.to_string()),
                token_hash: None,
            })?;

            Err(AppError::Zfs(e))
        }
    }
}

// ZFS list endpoint (read-only, no approval needed)
async fn zfs_list(
    State(state): State<AppState>,
    Extension(identity): Extension<ClientIdentity>,
    Json(req): Json<ListRequest>,
) -> Result<Json<ListResponse>, AppError> {
    state.metrics.increment_requests();
    state.metrics.increment_zfs_operation("list");

    let audit_id = uuid::Uuid::new_v4().to_string();

    // Log operation
    state.audit.log(AuditEvent {
        timestamp: chrono::Utc::now(),
        audit_id: audit_id.clone(),
        caller: identity.cn.clone(),
        caller_uid: identity.uid,
        operation: "zfs-list".to_string(),
        target: req.dataset.clone().unwrap_or_default(),
        approval_id: None,
        result: "attempting".to_string(),
        details: None,
        token_hash: None,
    })?;

    let result = state.zfs.list(req.dataset.as_deref(), req.list_type.as_deref(), &identity).await;

    match result {
        Ok(entries) => {
            state.audit.log(AuditEvent {
                timestamp: chrono::Utc::now(),
                audit_id: audit_id.clone(),
                caller: identity.cn.clone(),
                caller_uid: identity.uid,
                operation: "zfs-list".to_string(),
                target: req.dataset.clone().unwrap_or_default(),
                approval_id: None,
                result: "success".to_string(),
                details: Some(format!("{} entries", entries.len())),
                token_hash: None,
            })?;

            Ok(Json(ListResponse {
                success: true,
                entries,
                audit_id,
            }))
        }
        Err(e) => {
            state.audit.log(AuditEvent {
                timestamp: chrono::Utc::now(),
                audit_id: audit_id.clone(),
                caller: identity.cn.clone(),
                caller_uid: identity.uid,
                operation: "zfs-list".to_string(),
                target: req.dataset.clone().unwrap_or_default(),
                approval_id: None,
                result: "failure".to_string(),
                details: Some(e.to_string()),
                token_hash: None,
            })?;

            Err(AppError::Zfs(e))
        }
    }
}

// ZFS destroy endpoint — fixed control flow (P0-A2)
//
// Control flow:
//   1. requires_approval = false  →  execute immediately, return success
//   2. approval_token provided   →  validate + execute, return success/error
//   3. no token + requires_approval = true  →  create approval request, return pending
async fn zfs_destroy(
    State(state): State<AppState>,
    Extension(identity): Extension<ClientIdentity>,
    Json(req): Json<DestroyRequest>,
) -> Result<Json<DestroyResponse>, AppError> {
    state.metrics.increment_requests();

    // Rate-limit check (P-B5)
    if let Err(retry_after) = state.rate_limiter.check(&identity.cn) {
        state.metrics.increment_rate_limited();
        return Err(AppError::RateLimited(retry_after));
    }

    state.metrics.increment_zfs_operation("destroy");

    // Validate dataset/snapshot name
    if !zfs::is_valid_dataset_or_snapshot(&req.dataset) {
        return Err(AppError::InvalidDataset(req.dataset));
    }

    let config = state.config.get().await;
    let agent_cfg = config.find_agent(&identity.cn).cloned();
    let requires_approval = config.requires_approval_for_agent(
        "zfs-destroy",
        &req.dataset,
        agent_cfg.as_ref(),
    );

    // --- Branch 1: No approval required — execute directly ---
    if !requires_approval {
        let audit_id = uuid::Uuid::new_v4().to_string();

        state.audit.log(AuditEvent {
            timestamp: chrono::Utc::now(),
            audit_id: audit_id.clone(),
            caller: identity.cn.clone(),
            caller_uid: identity.uid,
            operation: "zfs-destroy".to_string(),
            target: req.dataset.clone(),
            approval_id: None,
            result: "attempting".to_string(),
            details: None,
            token_hash: None,
        })?;

        let result = state.zfs.execute(&req.dataset, ZfsOp::Destroy, &identity).await;

        return match result {
            Ok(output) => {
                state.audit.log(AuditEvent {
                    timestamp: chrono::Utc::now(),
                    audit_id: audit_id.clone(),
                    caller: identity.cn.clone(),
                    caller_uid: identity.uid,
                    operation: "zfs-destroy".to_string(),
                    target: req.dataset.clone(),
                    approval_id: None,
                    result: "success".to_string(),
                    details: Some(output),
                    token_hash: None,
                })?;
                Ok(Json(DestroyResponse::Success {
                    success: true,
                    audit_id,
                    message: format!("Destroyed {} (no approval required)", req.dataset),
                }))
            }
            Err(e) => {
                state.audit.log(AuditEvent {
                    timestamp: chrono::Utc::now(),
                    audit_id: audit_id.clone(),
                    caller: identity.cn.clone(),
                    caller_uid: identity.uid,
                    operation: "zfs-destroy".to_string(),
                    target: req.dataset.clone(),
                    approval_id: None,
                    result: "failure".to_string(),
                    details: Some(e.to_string()),
                    token_hash: None,
                })?;
                Err(AppError::Zfs(e))
            }
        };
    }

    // --- Branch 2: Approval required + token provided — validate and execute ---
    if let Some(ref token) = req.approval_token {
        let approval_id = state
            .approvals
            .validate_and_consume_token(token, &req.dataset, &identity.cn)
            .await;

        return match approval_id {
            Some(id) => {
                let audit_id = uuid::Uuid::new_v4().to_string();

                state.audit.log(AuditEvent {
                    timestamp: chrono::Utc::now(),
                    audit_id: audit_id.clone(),
                    caller: identity.cn.clone(),
                    caller_uid: identity.uid,
                    operation: "zfs-destroy".to_string(),
                    target: req.dataset.clone(),
                    approval_id: Some(id.clone()),
                    result: "attempting".to_string(),
                    details: None,
                    token_hash: None,
                })?;

                let result = state.zfs.execute(&req.dataset, ZfsOp::Destroy, &identity).await;

                match result {
                    Ok(output) => {
                        state.metrics.increment_approvals_granted();
                        state.audit.log(AuditEvent {
                            timestamp: chrono::Utc::now(),
                            audit_id: audit_id.clone(),
                            caller: identity.cn.clone(),
                            caller_uid: identity.uid,
                            operation: "zfs-destroy".to_string(),
                            target: req.dataset.clone(),
                            approval_id: Some(id),
                            result: "success".to_string(),
                            details: Some(output),
                            token_hash: None,
                        })?;
                        Ok(Json(DestroyResponse::Success {
                            success: true,
                            audit_id,
                            message: format!("Destroyed {}", req.dataset),
                        }))
                    }
                    Err(e) => {
                        state.audit.log(AuditEvent {
                            timestamp: chrono::Utc::now(),
                            audit_id: audit_id.clone(),
                            caller: identity.cn.clone(),
                            caller_uid: identity.uid,
                            operation: "zfs-destroy".to_string(),
                            target: req.dataset.clone(),
                            approval_id: Some(id),
                            result: "failure".to_string(),
                            details: Some(e.to_string()),
                            token_hash: None,
                        })?;
                        Err(AppError::Zfs(e))
                    }
                }
            }
            None => Err(AppError::InvalidToken),
        };
    }

    // --- Branch 3: Approval required, no token — create approval request and return pending ---
    let approval_req = ApprovalRequest {
        id: uuid::Uuid::new_v4().to_string(),
        caller: identity.cn.clone(),
        caller_uid: identity.uid,
        operation: "zfs-destroy".to_string(),
        target: req.dataset.clone(),
        requested_at: chrono::Utc::now(),
        nzc_request_id: None,
    };

    let token = state.approvals.create_approval(approval_req.clone()).await;
    state.metrics.increment_approvals_created();

    // Log pending approval (token hash only, P1-6)
    let token_audit = approval::token::TokenAuditInfo::from(token.as_str());
    state.audit.log(AuditEvent {
        timestamp: chrono::Utc::now(),
        audit_id: approval_req.id.clone(),
        caller: identity.cn.clone(),
        caller_uid: identity.uid,
        operation: "zfs-destroy".to_string(),
        target: req.dataset.clone(),
        approval_id: Some(approval_req.id.clone()),
        result: "pending_approval".to_string(),
        details: None,
        token_hash: Some(token_audit.hash),
    })?;

    Ok(Json(DestroyResponse::Pending {
        pending_approval: true,
        approval_id: approval_req.id,
        message: format!(
            "Approval required. Reply CONFIRM {} to approve (5 min timeout).",
            token_audit.masked
        ),
    }))
}

// Approve endpoint — submit approval token manually (P0-A3: identity-aware)
async fn submit_approval(
    State(state): State<AppState>,
    Extension(identity): Extension<ClientIdentity>,
    Json(req): Json<ApproveRequest>,
) -> Result<Json<ApproveResponse>, AppError> {
    state.metrics.increment_requests();

    // Rate-limit check (P-B5)
    if let Err(retry_after) = state.rate_limiter.check(&identity.cn) {
        state.metrics.increment_rate_limited();
        return Err(AppError::RateLimited(retry_after));
    }

    // P0-A3: Check if this operation is approval_admin_only
    let config = state.config.get().await;

    // Find the approval request to determine which operation is being approved
    // (We check rule-level approval_admin_only for zfs-destroy and similar operations)
    // For simplicity, check if ANY rule with approval_admin_only requires admin CN
    let is_admin_required = config.find_rule("zfs-destroy")
        .map(|r| r.approval_admin_only)
        .unwrap_or(false);

    if is_admin_required {
        // Check if caller CN matches admin_cn_pattern
        let admin_pattern = config.approval.admin_cn_pattern.as_deref().unwrap_or("");
        if !admin_pattern.is_empty() {
            let pattern_matches = if admin_pattern.ends_with('*') {
                identity.cn.starts_with(&admin_pattern[..admin_pattern.len()-1])
            } else {
                identity.cn == admin_pattern
            };

            if !pattern_matches {
                warn!(
                    cn = %identity.cn,
                    required_pattern = %admin_pattern,
                    "Approval rejected: caller CN does not match admin_cn_pattern"
                );
                state.metrics.increment_policy_denials();
                return Err(AppError::PolicyDenied(
                    format!("Approver identity '{}' does not match admin_cn_pattern", identity.cn)
                ));
            }
        }
    }

    // P-C7: Invoke identity plugin if configured
    if let Some(ref plugin) = config.approval.identity_plugin {
        let plugin_req = PluginRequest {
            approver_cn: identity.cn.clone(),
            approval_id: req.approval_id.clone(),
            operation: "approve".to_string(),
            target: req.approval_id.clone(),
        };

        match validate_approver_identity(plugin, &plugin_req).await {
            Ok(true) => {
                info!(cn = %identity.cn, "Identity plugin approved approver");
            }
            Ok(false) => {
                warn!(cn = %identity.cn, "Identity plugin denied approver");
                state.metrics.increment_policy_denials();
                return Err(AppError::PolicyDenied(
                    format!("Approver identity '{}' rejected by identity plugin", identity.cn)
                ));
            }
            Err(e) => {
                error!(cn = %identity.cn, error = %e, "Identity plugin invocation failed — denying (fail-closed)");
                state.metrics.increment_policy_denials();
                return Err(AppError::PolicyDenied(
                    "Identity plugin failed; approval denied (fail-closed)".to_string()
                ));
            }
        }
    }

    let timestamp = chrono::DateTime::parse_from_rfc3339(&chrono::Utc::now().to_rfc3339())
        .map_err(|_| AppError::Approval(error::ApprovalError::InvalidToken))?
        .with_timezone(&chrono::Utc);

    let payload = SignalWebhookPayload {
        token: req.token,
        confirmation_code: "CONFIRM".to_string(),
        approver: identity.cn.clone(),
        timestamp,
    };

    let result = state.approvals.handle_signal_confirmation(&payload).await;

    match result {
        Ok(_) => Ok(Json(ApproveResponse {
            success: true,
            message: "Approval granted. You may now execute the operation.".to_string(),
        })),
        Err(e) => Err(AppError::Approval(e)),
    }
}

// Signal webhook endpoint for approval confirmations
async fn signal_webhook(
    State(state): State<AppState>,
    Json(payload): Json<SignalWebhookBody>,
) -> Result<Json<ApproveResponse>, AppError> {
    state.metrics.increment_requests();

    let timestamp = chrono::DateTime::parse_from_rfc3339(&payload.timestamp)
        .map_err(|_| AppError::Approval(error::ApprovalError::InvalidToken))?
        .with_timezone(&chrono::Utc);

    let webhook_payload = SignalWebhookPayload {
        token: payload.token,
        confirmation_code: payload.confirmation_code,
        approver: payload.approver,
        timestamp,
    };

    let result = state.approvals.handle_signal_confirmation(&webhook_payload).await;

    match result {
        Ok(_) => Ok(Json(ApproveResponse {
            success: true,
            message: "Approval granted via Signal.".to_string(),
        })),
        Err(e) => Err(AppError::Approval(e)),
    }
}

// Pending approvals endpoint (filtered by caller identity, P1-7)
async fn list_pending(
    State(state): State<AppState>,
    Extension(identity): Extension<ClientIdentity>,
) -> impl IntoResponse {
    state.metrics.increment_requests();

    // Rate-limit check (P-B5)
    if let Err(retry_after) = state.rate_limiter.check(&identity.cn) {
        state.metrics.increment_rate_limited();
        return rate_limit_response(retry_after);
    }

    // Only show pending for the requesting caller
    let pending = state.approvals.list_pending_for_caller(&identity.cn).await;
    Json(pending).into_response()
}

// Admin pending endpoint — all pending (could be restricted by admin role)
async fn list_all_pending(State(state): State<AppState>) -> impl IntoResponse {
    state.metrics.increment_requests();
    let pending = state.approvals.list_all_pending().await;
    Json(pending)
}

// Admin permission-warning probe — GET /admin/warn-permissions
//
// Scans /etc/sudoers and /etc/sudoers.d/* for risky patterns that indicate
// the clash-agent user has overly-broad sudo privileges.  Results are also
// written to audit.jsonl and exposed in the Prometheus metrics endpoint.
//
// This endpoint is admin-only (caller CN must match admin_cn_pattern if set).
async fn warn_permissions(
    State(state): State<AppState>,
    Extension(identity): Extension<ClientIdentity>,
) -> impl IntoResponse {
    state.metrics.increment_requests();

    // Admin-only guard (mirrors list_all_pending logic)
    let config = state.config.get().await;
    if let Some(ref pattern) = config.approval.admin_cn_pattern {
        if !pattern.is_empty() {
            let matches = if pattern.ends_with('*') {
                identity.cn.starts_with(&pattern[..pattern.len() - 1])
            } else {
                identity.cn == *pattern
            };
            if !matches {
                return (
                    axum::http::StatusCode::FORBIDDEN,
                    Json(serde_json::json!({
                        "error": "admin access required",
                        "caller": identity.cn,
                    })),
                ).into_response();
            }
        }
    }

    use std::sync::atomic::AtomicU64;
    let risky_gauge = Arc::new(AtomicU64::new(0));
    let result = perm_warn::probe_and_record(&state.audit, &state.metrics, &risky_gauge);

    let status = if result.risky_entries.is_empty() {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::OK // still 200; warnings are informational
    };

    (status, Json(result)).into_response()
}

// Unified operation dispatch endpoint — POST /host/op
//
// Takes a HostOp JSON body, dispatches to the appropriate adapter,
// runs policy validation, then executes if approved.
//
// Flow:
//   1. Look up adapter by HostOp::kind
//   2. Call adapter.validate() → PolicyDecision
//   3. If RequiresApproval:
//      a. If approval_token in metadata → try to consume it
//      b. Otherwise → create approval request and return pending
//   4. Call adapter.execute() → ExecutionResult
//   5. Audit + return result
async fn host_op_dispatch(
    State(state): State<AppState>,
    Extension(identity): Extension<ClientIdentity>,
    Json(op): Json<HostOp>,
) -> Result<impl IntoResponse, AppError> {
    state.metrics.increment_requests();

    // Rate-limit check
    if let Err(retry_after) = state.rate_limiter.check(&identity.cn) {
        state.metrics.increment_rate_limited();
        return Err(AppError::RateLimited(retry_after));
    }

    let audit_id = uuid::Uuid::new_v4().to_string();
    let operation_label = format!("{}/{}", op.kind, op.command().unwrap_or("unknown"));

    // Find adapter
    let adapter = state
        .adapter_registry
        .dispatch(&op.kind)
        .ok_or_else(|| AppError::Internal(format!("No adapter registered for kind '{}'", op.kind)))?;

    // Audit: operation attempt
    state.audit.log(AuditEvent {
        timestamp: chrono::Utc::now(),
        audit_id: audit_id.clone(),
        caller: identity.cn.clone(),
        caller_uid: identity.uid,
        operation: operation_label.clone(),
        target: op.resource.clone().unwrap_or_default(),
        approval_id: None,
        result: "attempting".to_string(),
        details: None,
        token_hash: None,
    })?;

    // Adapter validation + policy decision
    let decision = adapter.validate(&state, &op).await?;

    match decision {
        PolicyDecision::Deny { reason } => {
            state.metrics.increment_policy_denials();
            state.audit.log(AuditEvent {
                timestamp: chrono::Utc::now(),
                audit_id: audit_id.clone(),
                caller: identity.cn.clone(),
                caller_uid: identity.uid,
                operation: operation_label.clone(),
                target: op.resource.clone().unwrap_or_default(),
                approval_id: None,
                result: "denied".to_string(),
                details: Some(reason.clone()),
                token_hash: None,
            })?;
            return Err(AppError::PolicyDenied(reason));
        }

        PolicyDecision::RequiresApproval { message: _ } => {
            // Check if approval token was provided
            if let Some(token) = op.approval_token() {
                let approval_id = state
                    .approvals
                    .validate_and_consume_token(token, op.resource.as_deref().unwrap_or(""), &identity.cn)
                    .await;

                match approval_id {
                    Some(id) => {
                        // Token valid — fall through to execute
                        state.audit.log(AuditEvent {
                            timestamp: chrono::Utc::now(),
                            audit_id: audit_id.clone(),
                            caller: identity.cn.clone(),
                            caller_uid: identity.uid,
                            operation: operation_label.clone(),
                            target: op.resource.clone().unwrap_or_default(),
                            approval_id: Some(id),
                            result: "approval_consumed".to_string(),
                            details: None,
                            token_hash: None,
                        })?;
                        // Continue to execute below
                    }
                    None => {
                        return Err(AppError::InvalidToken);
                    }
                }
            } else {
                // No token — create approval request and return pending
                let approval_req = ApprovalRequest {
                    id: uuid::Uuid::new_v4().to_string(),
                    caller: identity.cn.clone(),
                    caller_uid: identity.uid,
                    operation: operation_label.clone(),
                    target: op.resource.clone().unwrap_or_default(),
                    requested_at: chrono::Utc::now(),
                    nzc_request_id: None,
                };

                let token = state.approvals.create_approval(approval_req.clone()).await;
                state.metrics.increment_approvals_created();

                let token_audit = approval::token::TokenAuditInfo::from(token.as_str());
                state.audit.log(AuditEvent {
                    timestamp: chrono::Utc::now(),
                    audit_id: approval_req.id.clone(),
                    caller: identity.cn.clone(),
                    caller_uid: identity.uid,
                    operation: operation_label.clone(),
                    target: op.resource.clone().unwrap_or_default(),
                    approval_id: Some(approval_req.id.clone()),
                    result: "pending_approval".to_string(),
                    details: None,
                    token_hash: Some(token_audit.hash),
                })?;

                return Ok(Json(serde_json::json!({
                    "pending_approval": true,
                    "approval_id": approval_req.id,
                    "message": format!(
                        "Approval required for {operation_label}. Reply CONFIRM {} to approve.",
                        token_audit.masked
                    ),
                })).into_response());
            }
        }

        PolicyDecision::Allow => {
            // Fall through to execute
        }
    }

    // Execute
    let result = adapter.execute(&state, &identity, &op).await;

    match result {
        Ok(exec_result) => {
            state.audit.log(AuditEvent {
                timestamp: chrono::Utc::now(),
                audit_id: audit_id.clone(),
                caller: identity.cn.clone(),
                caller_uid: identity.uid,
                operation: operation_label.clone(),
                target: op.resource.clone().unwrap_or_default(),
                approval_id: None,
                result: "success".to_string(),
                details: Some(exec_result.output.clone()),
                token_hash: None,
            })?;

            Ok(Json(serde_json::json!({
                "success": true,
                "audit_id": audit_id,
                "output": exec_result.output,
                "exit_code": exec_result.exit_code,
                "metadata": exec_result.metadata,
            })).into_response())
        }
        Err(e) => {
            state.audit.log(AuditEvent {
                timestamp: chrono::Utc::now(),
                audit_id: audit_id.clone(),
                caller: identity.cn.clone(),
                caller_uid: identity.uid,
                operation: operation_label.clone(),
                target: op.resource.clone().unwrap_or_default(),
                approval_id: None,
                result: "failure".to_string(),
                details: Some(e.to_string()),
                token_hash: None,
            })?;
            Err(e)
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Install ring crypto provider for rustls
    let _ = default_provider().install_default();

    let cli = Cli::parse();

    // Initialize tracing
    let _subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| if cli.debug { "debug" } else { "info" }.into()),
        )
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .init();

    info!("Starting ZeroClawed Host-Agent v{}", env!("CARGO_PKG_VERSION"));

    // Load configuration
    let config = Config::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    info!("Loaded configuration from {:?}", cli.config);

    let reloadable_config = ReloadableConfig::new(config.clone(), cli.config.to_string_lossy().to_string());

    // Initialize Signal client if configured
    let signal_client = if let Some(webhook) = &config.approval.signal_webhook {
        Some(approval::signal::SignalClient::new(
            webhook.clone(),
            config.approval.allowed_approvers.clone(),
        ))
    } else {
        None
    };

    // Initialize components
    let audit = AuditLogger::new(
        &config.audit.log_path,
        RotationStrategy::from(config.audit.rotation.as_str()),
        config.audit.retention_days,
    ).with_context(|| "Failed to initialize audit logger")?;

    let approvals = ApprovalManager::new(config.approval.ttl_seconds, signal_client);
    let zfs = ZfsExecutor::new();
    let metrics = Arc::new(Metrics::new());
    let agent_registry = Arc::new(AgentRegistry::new(config.agents.clone()));
    let rate_limiter = Arc::new(RateLimiter::new(&config.rate_limit));

    // Spawn background task to evict expired rate-limit buckets every 5 minutes
    {
        let rl_clone = rate_limiter.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(300));
            loop {
                interval.tick().await;
                rl_clone.evict_expired();
            }
        });
    }

    // Build adapter registry
    let adapter_registry = AdapterRegistry::new()
        .with(adapters::zfs::ZfsAdapter::new())
        .with(adapters::systemd::SystemdAdapter::new())
        .with(adapters::pct::PctAdapter::new())
        .with(adapters::git::GitAdapter::new())
        .with(adapters::exec::ExecAdapter::new());

    info!("Registered adapters: {:?}", adapter_registry.kinds());

    let state = AppState {
        config: reloadable_config,
        audit: Arc::new(audit),
        approvals: Arc::new(approvals),
        zfs: Arc::new(zfs),
        metrics,
        agent_registry,
        rate_limiter,
        adapter_registry: Arc::new(adapter_registry),
    };

    // Build router with state
    let app = Router::new()
        .route("/health", get(health_check))
        .route("/metrics", get(metrics::metrics_handler))
        // Unified adapter dispatch endpoint (v4)
        .route("/host/op", post(host_op_dispatch))
        // Legacy ZFS endpoints (kept for backwards compatibility — shim to adapter)
        .route("/zfs/snapshot", post(zfs_snapshot))
        .route("/zfs/list", post(zfs_list))
        .route("/zfs/destroy", post(zfs_destroy))
        .route("/approve", post(submit_approval))
        .route("/webhook/signal", post(signal_webhook))
        .route("/pending", get(list_pending))
        .route("/admin/pending", get(list_all_pending))
        .route("/admin/warn-permissions", get(warn_permissions))
        .with_state(state);

    // Setup mTLS — NO HTTP FALLBACK (P0-2)
    let addr: SocketAddr = config.server.bind.parse()?;
    info!("Setting up mTLS on {}", addr);

    let tls_config = tls::create_mtls_config(
        &config.server.cert,
        &config.server.key,
        &config.server.client_ca,
        config.server.crl_file.as_ref(),
    ).with_context(|| "Failed to create mTLS configuration")?;

    info!("mTLS configuration created successfully");

    // Create TCP listener
    let listener = TcpListener::bind(addr).await
        .with_context(|| format!("Failed to bind to {}", addr))?;

    info!("Bound to {}", addr);

    // Create identity-extracting acceptor — this is what wires ClientIdentity into
    // request extensions (P0-A1 fix). We drive our own accept loop so we can call
    // acceptor.accept() which returns (ClientIdentity, TlsStream) and inject the
    // identity into the request extensions before dispatching to the axum router.
    let acceptor = Arc::new(IdentityExtractingAcceptor::new(
        tls_config,
        None, // CRL already checked in create_mtls_config
    ));

    info!("Host-Agent ready with mTLS enforcement + identity injection");

    // Spawn metrics server if enabled
    if config.metrics.enabled {
        let metrics_addr: SocketAddr = config.metrics.bind.parse()?;
        tokio::spawn(async move {
            let metrics_app = Router::new()
                .route("/metrics", get(|| async { "Metrics endpoint" }));

            let listener = match TcpListener::bind(metrics_addr).await {
                Ok(l) => l,
                Err(e) => {
                    warn!("Failed to bind metrics server: {}", e);
                    return;
                }
            };

            info!("Metrics server listening on {}", metrics_addr);

            if let Err(e) = axum::serve(listener, metrics_app).await {
                warn!("Metrics server error: {}", e);
            }
        });
    }

    // Custom accept loop: for each connection, TLS-handshake → extract ClientIdentity
    // → inject into request extensions → serve with hyper + axum.
    //
    // This replaces axum_server::bind_rustls which does not support custom per-connection
    // extension injection. Using hyper_util::server::conn::auto::Builder for H1+H2 support.
    info!("Server running. Ctrl+C to stop.");

    let shutdown_token = tokio_util::sync::CancellationToken::new();
    let shutdown_clone = shutdown_token.clone();

    tokio::spawn(async move {
        if let Err(e) = tokio::signal::ctrl_c().await {
            error!("Failed to install Ctrl+C handler: {}", e);
        }
        info!("Received shutdown signal, stopping gracefully...");
        shutdown_clone.cancel();
    });

    loop {
        tokio::select! {
            _ = shutdown_token.cancelled() => {
                info!("Host-Agent stopped");
                break;
            }
            accept_result = listener.accept() => {
                let (tcp_stream, peer_addr) = match accept_result {
                    Ok(v) => v,
                    Err(e) => {
                        error!("TCP accept failed: {}", e);
                        continue;
                    }
                };

                let acceptor = acceptor.clone();
                let app = app.clone();

                tokio::spawn(async move {
                    // TLS handshake + client certificate extraction
                    let (identity, tls_stream) = match acceptor.accept(tcp_stream).await {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(peer = %peer_addr, "mTLS handshake failed: {}", e);
                            return;
                        }
                    };

                    tracing::debug!(
                        cn = %identity.cn,
                        uid = %identity.uid,
                        peer = %peer_addr,
                        "mTLS handshake complete — serving request"
                    );

                    // Clone identity to move into the service wrapper
                    let identity_for_service = identity.clone();

                    // Build a tower service that injects ClientIdentity into each request's
                    // extensions before passing to the axum router.
                    // hyper passes Request<Incoming>; axum Router accepts Request<Body>,
                    // so we convert via http_body_util::Limited or just use axum's body conversion.
                    let tower_svc = tower::service_fn(move |req: hyper::Request<Incoming>| {
                        let identity = identity_for_service.clone();
                        let mut inner = app.clone();
                        async move {
                            // Convert hyper::Request<Incoming> → axum::Request<Body>
                            let (parts, body) = req.into_parts();
                            let body = axum::body::Body::new(body);
                            let mut axum_req = hyper::Request::from_parts(parts, body);
                            axum_req.extensions_mut().insert(identity);
                            inner.call(axum_req).await
                        }
                    });

                    // Wrap the tower service in TowerToHyperService so hyper can use it
                    let service = TowerToHyperService::new(tower_svc);

                    // Serve the connection using hyper with H1+H2 auto-detection
                    let io = TokioIo::new(tls_stream);
                    if let Err(e) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                        .serve_connection(io, service)
                        .await
                    {
                        // Ignore harmless "connection reset by peer" / early EOF errors
                        let err_str = e.to_string();
                        if !err_str.contains("connection was reset")
                            && !err_str.contains("early eof")
                            && !err_str.contains("broken pipe")
                        {
                            warn!(peer = %peer_addr, "Connection error: {}", e);
                        }
                    }
                });
            }
        }
    }

    Ok(())
}
