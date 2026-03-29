//! Approval token management for destructive operations
//!
//! Integrates with NonZeroClaw policy engine for unified approvals (P3-16, P3-18).

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::approval::signal::{SignalClient, SignalWebhookPayload, ValidatedApproval};
use crate::approval::token::{hash_token, generate_token, TokenAuditInfo};
use crate::auth::ClientIdentity;

pub mod signal;
pub mod token;

/// An approval request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    #[serde(rename = "id")]
    pub id: String,
    pub caller: String,
    #[serde(rename = "caller_uid")]
    pub caller_uid: u32,
    pub operation: String,
    pub target: String,
    #[serde(rename = "requested_at")]
    pub requested_at: DateTime<Utc>,
    /// NZC request ID for cross-agent visibility (P3-18)
    pub nzc_request_id: Option<String>,
}

/// Response for pending approvals
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalResponse {
    #[serde(rename = "approval_id")]
    pub approval_id: String,
    /// Token hash for display (never the plaintext token)
    pub token_hash_prefix: String,
    pub caller: String,
    pub operation: String,
    pub target: String,
    #[serde(rename = "expires_at")]
    pub expires_at: DateTime<Utc>,
}

/// Internal token storage
#[derive(Debug, Clone)]
struct TokenEntry {
    request: ApprovalRequest,
    /// SHA-256 hash of the token (plaintext never stored)
    token_hash: String,
    expires_at: DateTime<Utc>,
    used: bool,
    approved: bool,
    /// Who approved this (Signal number for audit)
    approved_by: Option<String>,
}

/// Manages approval tokens in memory
pub struct ApprovalManager {
    tokens: Arc<RwLock<HashMap<String, TokenEntry>>>, // token_hash -> entry
    by_id: Arc<RwLock<HashMap<String, String>>>,      // approval_id -> token_hash
    ttl_seconds: i64,
    signal: Option<SignalClient>,
}

impl ApprovalManager {
    /// Create a new approval manager with specified TTL
    pub fn new(ttl_seconds: u64, signal: Option<SignalClient>) -> Self {
        let manager = Self {
            tokens: Arc::new(RwLock::new(HashMap::new())),
            by_id: Arc::new(RwLock::new(HashMap::new())),
            ttl_seconds: ttl_seconds as i64,
            signal,
        };

        // Start cleanup task
        let tokens_clone = manager.tokens.clone();
        let by_id_clone = manager.by_id.clone();
        tokio::spawn(async move {
            cleanup_task(tokens_clone, by_id_clone).await;
        });

        manager
    }

    /// Create a new approval request and return the token (P1-5: 16-char token)
    /// 
    /// Returns the plaintext token (give to user) and stores only the hash.
    pub async fn create_approval(&self, request: ApprovalRequest) -> String {
        // Generate high-entropy token
        let token = generate_token();
        let token_audit = TokenAuditInfo::from(token.as_str());
        let expires_at = Utc::now() + Duration::seconds(self.ttl_seconds);

        let entry = TokenEntry {
            request: request.clone(),
            token_hash: token_audit.hash.clone(),
            expires_at,
            used: false,
            approved: false,
            approved_by: None,
        };

        {
            let mut tokens = self.tokens.write().await;
            let mut by_id = self.by_id.write().await;

            tokens.insert(token_audit.hash.clone(), entry);
            by_id.insert(request.id.clone(), token_audit.hash.clone());
        }

        // Log only hash (P1-6)
        info!(
            approval_id = %request.id,
            caller = %request.caller,
            operation = %request.operation,
            target = %request.target,
            token_hash = %token_audit.hash,
            token_masked = %token_audit.masked,
            "Created approval request"
        );

        // Send Signal notification if configured
        if let Some(ref signal) = self.signal {
            let _ = signal.notify_approval_request(
                &token_audit,
                &request.caller,
                &request.operation,
                &request.target,
            ).await;
        }

        token
    }

    /// Validate a token and return the approval_id if valid
    /// 
    /// Note: token is consumed here to prevent replay attacks.
    pub async fn validate_and_consume_token(
        &self,
        token: &str,
        target: &str,
        caller: &str,
    ) -> Option<String> {
        let token_hash = hash_token(token);
        let mut tokens = self.tokens.write().await;

        if let Some(entry) = tokens.get_mut(&token_hash) {
            // Check expiration
            if Utc::now() > entry.expires_at {
                debug!(token_hash = %token_hash, "Token expired");
                return None;
            }

            // Check if used
            if entry.used {
                debug!(token_hash = %token_hash, "Token already used");
                return None;
            }

            // Check if approved (Signal confirmation required)
            if !entry.approved {
                debug!(token_hash = %token_hash, "Token not yet approved via Signal");
                return None;
            }

            // Verify target matches
            if entry.request.target != target {
                warn!(
                    token_hash = %token_hash,
                    expected = %entry.request.target,
                    got = %target,
                    "Token target mismatch"
                );
                return None;
            }

            // Verify caller matches
            if entry.request.caller != caller {
                warn!(
                    token_hash = %token_hash,
                    expected = %entry.request.caller,
                    got = %caller,
                    "Token caller mismatch"
                );
                return None;
            }

            // Mark as used
            entry.used = true;
            info!(
                token_hash = %token_hash,
                approval_id = %entry.request.id,
                "Token validated and consumed"
            );

            return Some(entry.request.id.clone());
        }

        None
    }

    /// Handle Signal webhook confirmation (P3-18)
    pub async fn handle_signal_confirmation(&self, payload: &SignalWebhookPayload) -> Result<(), crate::error::ApprovalError> {
        // Validate the callback
        let validation = self.signal
            .as_ref()
            .ok_or_else(|| crate::error::ApprovalError::NotFound("Signal not configured".to_string()))?
            .validate_callback(payload)
            .map_err(|e| crate::error::ApprovalError::InvalidToken)?;

        let token_hash = validation.token_hash;
        let mut tokens = self.tokens.write().await;

        if let Some(entry) = tokens.get_mut(&token_hash) {
            if entry.used {
                return Err(crate::error::ApprovalError::AlreadyUsed);
            }
            if entry.expires_at < Utc::now() {
                return Err(crate::error::ApprovalError::Expired(
                    payload.token.clone()
                ));
            }

            entry.approved = true;
            entry.approved_by = Some(validation.approver.clone());

            info!(
                approval_id = %entry.request.id,
                token_hash = %token_hash,
                approved_by = %validation.approver,
                "Approval granted via Signal"
            );

            Ok(())
        } else {
            Err(crate::error::ApprovalError::NotFound(
                format!("Token hash: {}", &token_hash[..8])
            ))
        }
    }

    /// List pending approvals for a specific caller (P1-7: filter by identity)
    pub async fn list_pending_for_caller(&self, caller: &str) -> Vec<ApprovalResponse> {
        let tokens = self.tokens.read().await;
        let now = Utc::now();

        tokens
            .values()
            .filter(|e| {
                !e.used && 
                !e.approved && 
                e.expires_at > now &&
                e.request.caller == caller // Filter by caller
            })
            .map(|e| ApprovalResponse {
                approval_id: e.request.id.clone(),
                token_hash_prefix: e.token_hash[..8].to_string(),
                caller: e.request.caller.clone(),
                operation: e.request.operation.clone(),
                target: e.request.target.clone(),
                expires_at: e.expires_at,
            })
            .collect()
    }

    /// List all pending approvals (admin only)
    pub async fn list_all_pending(&self) -> Vec<ApprovalResponse> {
        let tokens = self.tokens.read().await;
        let now = Utc::now();

        tokens
            .values()
            .filter(|e| !e.used && !e.approved && e.expires_at > now)
            .map(|e| ApprovalResponse {
                approval_id: e.request.id.clone(),
                token_hash_prefix: e.token_hash[..8].to_string(),
                caller: e.request.caller.clone(),
                operation: e.request.operation.clone(),
                target: e.request.target.clone(),
                expires_at: e.expires_at,
            })
            .collect()
    }

    /// Mark a token as used (for internal consumption)
    pub async fn mark_used(&self, token_hash: &str) {
        let mut tokens = self.tokens.write().await;
        if let Some(entry) = tokens.get_mut(token_hash) {
            entry.used = true;
            info!(
                token_hash = %token_hash,
                approval_id = %entry.request.id,
                "Token marked as used"
            );
        }
    }
}

/// Background cleanup task for expired tokens
async fn cleanup_task(
    tokens: Arc<RwLock<HashMap<String, TokenEntry>>>,
    by_id: Arc<RwLock<HashMap<String, String>>>,
) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));

    loop {
        interval.tick().await;

        let now = Utc::now();
        let mut tokens_guard = tokens.write().await;
        let mut by_id_guard = by_id.write().await;

        let expired: Vec<String> = tokens_guard
            .iter()
            .filter(|(_, e)| e.expires_at < now)
            .map(|(k, _)| k.clone())
            .collect();

        for token_hash in expired {
            if let Some(entry) = tokens_guard.remove(&token_hash) {
                by_id_guard.remove(&entry.request.id);
                debug!(
                    token_hash = %token_hash,
                    approval_id = %entry.request.id,
                    "Cleaned up expired token"
                );
            }
        }

        drop(tokens_guard);
        drop(by_id_guard);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manager() -> ApprovalManager {
        ApprovalManager::new(300, None)
    }

    #[tokio::test]
    async fn test_create_approval() {
        let manager = test_manager();
        let request = ApprovalRequest {
            id: "test-id".to_string(),
            caller: "librarian".to_string(),
            caller_uid: 1000,
            operation: "zfs-destroy".to_string(),
            target: "tank/media@old".to_string(),
            requested_at: Utc::now(),
            nzc_request_id: None,
        };

        let token = manager.create_approval(request).await;
        assert_eq!(token.len(), 16);
    }

    #[tokio::test]
    async fn test_validate_token_wrong_caller() {
        let manager = test_manager();
        let request = ApprovalRequest {
            id: "test-id".to_string(),
            caller: "librarian".to_string(),
            caller_uid: 1000,
            operation: "zfs-destroy".to_string(),
            target: "tank/media@old".to_string(),
            requested_at: Utc::now(),
            nzc_request_id: None,
        };

        let token = manager.create_approval(request).await;
        
        // Can't validate with wrong caller
        let result = manager.validate_and_consume_token(&token, "tank/media@old", "attacker").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_list_pending_filters_by_caller() {
        let manager = test_manager();
        
        // Create approval for librarian
        let request1 = ApprovalRequest {
            id: "id1".to_string(),
            caller: "librarian".to_string(),
            caller_uid: 1000,
            operation: "zfs-destroy".to_string(),
            target: "tank/media@old".to_string(),
            requested_at: Utc::now(),
            nzc_request_id: None,
        };
        manager.create_approval(request1).await;

        // Create approval for lucien
        let request2 = ApprovalRequest {
            id: "id2".to_string(),
            caller: "lucien".to_string(),
            caller_uid: 1001,
            operation: "zfs-destroy".to_string(),
            target: "tank/system@old".to_string(),
            requested_at: Utc::now(),
            nzc_request_id: None,
        };
        manager.create_approval(request2).await;

        // Librarian should only see their own
        let librarian_pending = manager.list_pending_for_caller("librarian").await;
        assert_eq!(librarian_pending.len(), 1);
        assert_eq!(librarian_pending[0].caller, "librarian");

        // Lucien should only see their own
        let lucien_pending = manager.list_pending_for_caller("lucien").await;
        assert_eq!(lucien_pending.len(), 1);
        assert_eq!(lucien_pending[0].caller, "lucien");
    }
}
