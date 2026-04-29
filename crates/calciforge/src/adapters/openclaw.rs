//! ZeroClaw-compatible HTTP adapter.
//!
//! OpenClaw chat integration uses `openclaw-channel` in
//! [`super::openclaw_channel`]. The old OpenAI-compatible OpenClaw chat adapter
//! was removed because it bypassed channel/plugin semantics and did not provide
//! reliable slash-command behavior.

use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::Mutex;

use crate::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::{AdapterError, AgentAdapter, DispatchContext, RuntimeStatus};

// ---------------------------------------------------------------------------
// ZeroClawHttpAdapter — ZeroClaw native /webhook protocol
// ---------------------------------------------------------------------------

/// Metadata tracked by Calciforge for a pending Clash approval.
///
/// Created when ZeroClaw's `/webhook` returns `approval_required`.
/// Consumed when the human sends `!approve` or `!deny`.
#[derive(Debug, Clone)]
pub struct PendingApprovalMeta {
    /// The request ID used to key the approval in ZeroClaw.
    pub request_id: String,
    /// ZeroClaw base URL (for calling `/webhook/approve`).
    pub zeroclaw_endpoint: String,
    /// Bearer token for the ZeroClaw endpoint.
    pub zeroclaw_auth_token: String,
    /// Human-readable summary for display.
    pub _summary: String,
}

/// Shared map of pending approvals: request_id → PendingApprovalMeta.
pub type SharedPendingApprovals = Arc<Mutex<HashMap<String, PendingApprovalMeta>>>;

/// Adapter for ZeroClaw-compatible agents using the native `/webhook` endpoint.
///
/// This adapter calls ZeroClaw's native webhook endpoint which runs the full
/// agent loop (tools, memory, workspace) directly.
///
/// Request:  POST /webhook  {"message": "..."}
/// Response: {"response": "...", "status": "ok"}  (or {"error": "..."})
/// Special:  {"approval_required": {...}} — Calciforge notifies user, polls for result.
pub struct ZeroClawHttpAdapter {
    client: reqwest::Client,
    endpoint: String,
    auth_token: String,
    /// Shared pending approvals tracked across all ZeroClaw interactions.
    pub pending_approvals: SharedPendingApprovals,
}

impl ZeroClawHttpAdapter {
    pub fn new(endpoint: String, auth_token: String, _timeout_ms: Option<u64>) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client");
        Self {
            client,
            endpoint,
            auth_token,
            pending_approvals: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[derive(Serialize)]
struct ZeroClawWebhookRequest<'a> {
    message: &'a str,
    /// Resolved Calciforge identity name (e.g. "brian"). Omitted when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    sender: Option<&'a str>,
    /// Optional model override (used for alloy routing).
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
}

/// Wire-level approval request from ZeroClaw (deserialized from `/webhook` response).
#[derive(Debug, Deserialize, Clone)]
struct ZeroClawApprovalRequiredWire {
    request_id: String,
    reason: String,
    command: String,
}

#[derive(Deserialize)]
struct ZeroClawWebhookResponse {
    #[serde(default)]
    response: Option<String>,
    #[serde(default)]
    error: Option<String>,
    /// Present when the agent loop paused for human approval.
    #[serde(default)]
    approval_required: Option<ZeroClawApprovalRequiredWire>,
}

#[async_trait]
impl AgentAdapter for ZeroClawHttpAdapter {
    async fn dispatch(&self, msg: &str) -> Result<String, AdapterError> {
        self.dispatch_with_context(DispatchContext::message_only(msg))
            .await
    }

    async fn dispatch_with_context(
        &self,
        ctx: DispatchContext<'_>,
    ) -> Result<String, AdapterError> {
        let url = format!("{}/webhook", self.endpoint.trim_end_matches('/'));
        info!(
            endpoint = %url,
            sender = ?ctx.sender,
            "zeroclaw-http dispatch"
        );

        let body = ZeroClawWebhookRequest {
            message: ctx.message,
            sender: ctx.sender,
            model: ctx.model_override,
        };

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.auth_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    AdapterError::Timeout
                } else {
                    AdapterError::Unavailable(e.to_string())
                }
            })?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            warn!(status = %status, body = %body, "zeroclaw-http error response");
            return Err(AdapterError::Protocol(format!(
                "ZeroClaw returned HTTP {status}: {body}"
            )));
        }

        let zeroclaw_resp: ZeroClawWebhookResponse = resp
            .json()
            .await
            .map_err(|e| AdapterError::Protocol(format!("zeroclaw-http JSON parse error: {e}")))?;

        if let Some(err) = zeroclaw_resp.error {
            return Err(AdapterError::Protocol(format!(
                "ZeroClaw agent error: {err}"
            )));
        }

        // ── Clash approval flow ───────────────────────────────────────────────
        // When ZeroClaw's policy engine returns a `Review` verdict, the webhook
        // responds immediately with `approval_required` instead of a final
        // response.  Calciforge stores the pending approval and returns
        // `ApprovalPending` so the router can notify the user.
        if let Some(approval) = zeroclaw_resp.approval_required {
            let request_id = approval.request_id.clone();
            let summary = format!(
                "🔒 Approval required\nCommand: {}\nReason: {}\nReply !approve or !deny [reason]\nRequest ID: {}",
                approval.command, approval.reason, approval.request_id
            );

            // Track in the per-adapter pending store.
            self.pending_approvals.lock().await.insert(
                request_id.clone(),
                PendingApprovalMeta {
                    request_id: request_id.clone(),
                    zeroclaw_endpoint: self.endpoint.clone(),
                    zeroclaw_auth_token: self.auth_token.clone(),
                    _summary: summary.clone(),
                },
            );

            info!(
                request_id = %request_id,
                command = %approval.command,
                "zeroclaw-http: approval required — notifying user"
            );

            // Return ApprovalPending so the router sends the notification and
            // waits for the user's !approve / !deny command.
            return Err(AdapterError::ApprovalPending(
                crate::adapters::ZeroClawApprovalRequest {
                    request_id: approval.request_id,
                    reason: approval.reason,
                    command: approval.command,
                },
            ));
        }
        // ─────────────────────────────────────────────────────────────────────

        Ok(zeroclaw_resp.response.unwrap_or_default())
    }

    fn kind(&self) -> &'static str {
        "zeroclaw-http"
    }

    /// Query ZeroClaw runtime status via GET /v1/status endpoint.
    ///
    /// Returns runtime provider/model info including alloy constituents.
    /// Returns None if ZeroClaw doesn't support the endpoint (backward compatible).
    async fn get_runtime_status(&self) -> Option<RuntimeStatus> {
        let url = format!("{}/v1/status", self.endpoint.trim_end_matches('/'));

        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.auth_token)
            .send()
            .await
            .ok()?;

        if !resp.status().is_success() {
            return None;
        }

        #[derive(Deserialize)]
        struct ZeroClawStatusResponse {
            default_provider: String,
            #[serde(rename = "default_model")]
            _default_model: String,
            alloy_constituents: Option<Vec<(String, String)>>,
        }

        let status: ZeroClawStatusResponse = resp.json().await.ok()?;

        // Check if this is an alloy by looking at constituents
        let is_alloy = status.alloy_constituents.is_some();

        Some(RuntimeStatus {
            provider: if is_alloy {
                "alloy".to_string()
            } else {
                status.default_provider.clone()
            },
            model: status.default_provider, // This is the alias name (e.g., "fast-alloy")
            alloy_constituents: status.alloy_constituents,
            _last_selected: None, // ZeroClaw could add this later
        })
    }
}

/// Request body for ZeroClaw's `POST /webhook/approve` endpoint.
#[derive(Serialize)]
struct ZeroClawApproveRequest<'a> {
    request_id: &'a str,
    approved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
}

impl ZeroClawHttpAdapter {
    /// Send an approve or deny signal to ZeroClaw for a pending approval.
    ///
    /// Called by the `!approve` / `!deny` command handler.
    pub async fn send_approval_decision(
        client: &reqwest::Client,
        zeroclaw_endpoint: &str,
        zeroclaw_auth_token: &str,
        request_id: &str,
        approved: bool,
        reason: Option<&str>,
    ) -> Result<(), AdapterError> {
        let url = format!(
            "{}/webhook/approve",
            zeroclaw_endpoint.trim_end_matches('/')
        );
        let body = ZeroClawApproveRequest {
            request_id,
            approved,
            reason,
        };

        let resp = client
            .post(&url)
            .bearer_auth(zeroclaw_auth_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| AdapterError::Unavailable(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AdapterError::Protocol(format!(
                "ZeroClaw /webhook/approve returned HTTP {status}: {body}"
            )));
        }
        Ok(())
    }

    /// Poll ZeroClaw's `/webhook/result/{id}` for the continuation result.
    ///
    /// Blocks until the result is available or the timeout elapses.
    pub async fn poll_result(
        _client: &reqwest::Client,
        zeroclaw_endpoint: &str,
        zeroclaw_auth_token: &str,
        request_id: &str,
    ) -> Result<String, AdapterError> {
        let url = format!(
            "{}/webhook/result/{}",
            zeroclaw_endpoint.trim_end_matches('/'),
            request_id
        );

        // ZeroClaw's long-poll endpoint blocks up to ~10 minutes.
        // We give our HTTP client up to 12 minutes to accommodate.
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(720)) // 12 minutes
            .build()
            .expect("reqwest client for polling");

        let resp = client
            .get(&url)
            .bearer_auth(zeroclaw_auth_token)
            .send()
            .await
            .map_err(|e| AdapterError::Unavailable(e.to_string()))?;

        if resp.status() == reqwest::StatusCode::REQUEST_TIMEOUT {
            return Err(AdapterError::Timeout);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AdapterError::Protocol(format!(
                "ZeroClaw /webhook/result returned HTTP {status}: {body}"
            )));
        }

        #[derive(Deserialize)]
        struct ResultResponse {
            response: Option<String>,
        }

        let result: ResultResponse = resp
            .json()
            .await
            .map_err(|e| AdapterError::Protocol(format!("result JSON parse error: {e}")))?;

        Ok(result.response.unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zeroclaw_webhook_request_serialization_without_sender() {
        let req = ZeroClawWebhookRequest {
            message: "hello",
            sender: None,
            model: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["message"], "hello");
        // sender should be omitted entirely when None
        assert!(
            json.get("sender").is_none(),
            "sender should be absent when None"
        );
    }

    #[test]
    fn test_zeroclaw_webhook_request_serialization_with_sender() {
        let req = ZeroClawWebhookRequest {
            message: "hi",
            sender: Some("brian"),
            model: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["message"], "hi");
        assert_eq!(json["sender"], "brian");
    }

    #[test]
    fn test_dispatch_context_message_only() {
        let ctx = DispatchContext::message_only("test message");
        assert_eq!(ctx.message, "test message");
        assert!(ctx.sender.is_none());
    }

    #[test]
    fn test_zeroclaw_kind_is_zeroclaw_http() {
        let adapter = ZeroClawHttpAdapter::new(
            "http://127.0.0.1:18799".to_string(),
            "tok".to_string(),
            None,
        );
        assert_eq!(adapter.kind(), "zeroclaw-http");
    }
}
