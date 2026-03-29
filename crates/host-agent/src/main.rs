//! PolyClaw v3 Host-Agent
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
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::signal;
use tracing::{error, info, warn};

// Install rustls crypto provider early
use rustls::crypto::ring::default_provider;
use rustls::crypto::CryptoProvider;

mod approval;
mod audit;
mod auth;
mod config;
mod error;
mod metrics;
mod tls;
mod zfs;

use approval::{ApprovalManager, ApprovalRequest, signal::SignalWebhookPayload};
use audit::{AuditEvent, AuditLogger};
use auth::{AgentRegistry, ClientIdentity};
use config::{Config, ReloadableConfig, RotationStrategy};
use error::AppError;
use metrics::Metrics;
use tls::IdentityExtractingAcceptor;
use zfs::{ZfsEntry, ZfsExecutor, ZfsOp};

/// PolyClaw Host-Agent CLI
#[derive(Parser, Debug)]
#[command(name = "clash-host-agent")]
#[command(about = "PolyClaw v3 Host-Agent — mTLS RPC server for host delegation")]
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

// Health check endpoint
async fn health_check(State(state): State<AppState>) -> impl IntoResponse {
    state.metrics.increment_requests();
    Json(serde_json::json!({
        "status": "healthy",
        "version": env!("CARGO_PKG_VERSION"),
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

    // Check if approval required (P0-4)
    let config = state.config.get().await;
    if config.requires_approval("zfs-snapshot", &snapshot) {
        // This shouldn't happen for snapshots per default config, but check anyway
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

// ZFS destroy endpoint (requires approval)
async fn zfs_destroy(
    State(state): State<AppState>,
    Extension(identity): Extension<ClientIdentity>,
    Json(req): Json<DestroyRequest>,
) -> Result<Json<DestroyResponse>, AppError> {
    state.metrics.increment_requests();
    state.metrics.increment_zfs_operation("destroy");

    // Validate dataset/snapshot name
    if !zfs::is_valid_dataset_or_snapshot(&req.dataset) {
        return Err(AppError::InvalidDataset(req.dataset));
    }

    // Check if approval required (P0-4)
    let config = state.config.get().await;
    let requires_approval = config.requires_approval("zfs-destroy", &req.dataset);

    // If approval token provided, validate and execute
    if let Some(token) = req.approval_token {
        if requires_approval {
            // Validate and consume token in one operation
            let approval_id = state
                .approvals
                .validate_and_consume_token(&token, &req.dataset, &identity.cn)
                .await;

            match approval_id {
                Some(id) => {
                    // Execute destroy
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
                        token_hash: None, // Token already consumed, don't log again
                    })?;

                    let result = state
                        .zfs
                        .execute(&req.dataset, ZfsOp::Destroy, &identity)
                        .await;

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

                            return Ok(Json(DestroyResponse::Success {
                                success: true,
                                audit_id,
                                message: format!("Destroyed {}", req.dataset),
                            }));
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

                            return Err(AppError::Zfs(e));
                        }
                    }
                }
                None => {
                    return Err(AppError::InvalidToken);
                }
            }
        } else {
            // No approval required, execute directly
            let audit_id = uuid::Uuid::new_v4().to_string();
            let result = state
                .zfs
                .execute(&req.dataset, ZfsOp::Destroy, &identity)
                .await;

            match result {
                Ok(_) => Ok(Json(DestroyResponse::Success {
                    success: true,
                    audit_id,
                    message: format!("Destroyed {}", req.dataset),
                })),
                Err(e) => Err(AppError::Zfs(e)),
            }
        }
    }

    // No token provided — create approval request (if required)
    if !requires_approval {
        return Err(AppError::PolicyDenied(
            "Approval token required even though policy doesn't require it".to_string()
        ));
    }

    let approval_req = ApprovalRequest {
        id: uuid::Uuid::new_v4().to_string(),
        caller: identity.cn.clone(),
        caller_uid: identity.uid,
        operation: "zfs-destroy".to_string(),
        target: req.dataset.clone(),
        requested_at: chrono::Utc::now(),
        nzc_request_id: None, // TODO: integrate with NZC
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

// Approve endpoint — submit approval token manually
async fn submit_approval(
    State(state): State<AppState>,
    Json(req): Json<ApproveRequest>,
) -> Result<Json<ApproveResponse>, AppError> {
    state.metrics.increment_requests();

    // This would be called by Signal webhook in production
    // For now, it's a manual API endpoint

    // Parse the timestamp for the webhook payload
    let timestamp = chrono::DateTime::parse_from_rfc3339(&chrono::Utc::now().to_rfc3339())
        .map_err(|_| AppError::Approval(error::ApprovalError::InvalidToken))?
        .with_timezone(&chrono::Utc);

    let payload = SignalWebhookPayload {
        token: req.token,
        confirmation_code: "CONFIRM".to_string(),
        approver: "api".to_string(),
        timestamp,
    };

    let result = state
        .approvals
        .handle_signal_confirmation(&payload)
        .await;

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

    let result = state
        .approvals
        .handle_signal_confirmation(&webhook_payload)
        .await;

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

    // Only show pending for the requesting caller
    let pending = state.approvals.list_pending_for_caller(&identity.cn).await;
    Json(pending)
}

// Admin pending endpoint — all pending (could be restricted by admin role)
async fn list_all_pending(State(state): State<AppState>) -> impl IntoResponse {
    state.metrics.increment_requests();
    let pending = state.approvals.list_all_pending().await;
    Json(pending)
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

    info!("Starting PolyClaw Host-Agent v{}", env!("CARGO_PKG_VERSION"));

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

    let state = AppState {
        config: reloadable_config,
        audit: Arc::new(audit),
        approvals: Arc::new(approvals),
        zfs: Arc::new(zfs),
        metrics,
        agent_registry,
    };

    // Build router with state
    let app = Router::new()
        .route("/health", get(health_check))
        .route("/metrics", get(metrics::metrics_handler))
        .route("/zfs/snapshot", post(zfs_snapshot))
        .route("/zfs/list", post(zfs_list))
        .route("/zfs/destroy", post(zfs_destroy))
        .route("/approve", post(submit_approval))
        .route("/webhook/signal", post(signal_webhook))
        .route("/pending", get(list_pending))
        .route("/admin/pending", get(list_all_pending))
        .with_state(state);

    // Setup mTLS — NO HTTP FALLBACK (P0-2)
    let addr: SocketAddr = config.server.bind.parse()?;

    info!("Setting up mTLS on {}", addr);

    let tls_config = tls::create_mtls_config(
        &config.server.cert,
        &config.server.key,
        &config.server.client_ca,
        config.server.crl_file.as_deref(),
    ).with_context(|| "Failed to create mTLS configuration")?;

    info!("mTLS configuration created successfully");

    // Create TCP listener
    let listener = TcpListener::bind(addr).await
        .with_context(|| format!("Failed to bind to {}", addr))?;

    info!("Bound to {}", addr);

    // Create identity-extracting acceptor
    let acceptor = IdentityExtractingAcceptor::new(
        tls_config,
        None, // CRL already checked in create_mtls_config
    );

    info!("Host-Agent ready with mTLS enforcement");

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

    // Setup graceful shutdown
    let shutdown = signal::ctrl_c();

    // Accept connections and serve with mTLS
    // Note: This is a simplified version - production would use a custom accept loop
    // with IdentityExtractingAcceptor
    info!("Server running. Press Ctrl+C to stop.");

    // For now, use standard axum-server with the regular acceptor
    // The full IdentityExtractingAcceptor integration requires a custom accept loop
    let tls_config = tls::create_mtls_config(
        &config.server.cert,
        &config.server.key,
        &config.server.client_ca,
        config.server.crl_file.as_deref(),
    )?;

    let rustls_config = axum_server::tls_rustls::RustlsConfig::from_config(tls_config);

    // Setup graceful shutdown handler
    let handle = axum_server::Handle::new();
    let shutdown_handle = handle.clone();

    tokio::spawn(async move {
        signal::ctrl_c().await.expect("Failed to install Ctrl+C handler");
        info!("Received shutdown signal, stopping gracefully...");
        shutdown_handle.shutdown();
    });

    // Start server with mTLS only (P0-2: no HTTP fallback)
    axum_server::bind_rustls(addr, rustls_config)
        .handle(handle)
        .serve(app.into_make_service())
        .await
        .with_context(|| "Server error")?;

    info!("Host-Agent stopped");
    Ok(())
}
