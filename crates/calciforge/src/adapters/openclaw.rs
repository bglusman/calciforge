//! OpenClawHttpAdapter — dispatches to OpenAI-compatible `/v1/chat/completions`.
//!
//! Used for OpenClaw agents (Librarian, Custodian) that expose an OpenAI-compat
//! HTTP endpoint. Bearer token comes from the per-agent `api_key` / `auth_token`
//! config field, or the `CALCIFORGE_AGENT_TOKEN` env var as fallback.
//!
//! # Streaming
//!
//! This adapter sends `stream: true` and consumes the SSE response, collecting
//! content deltas into a single String. The `dispatch()` interface is unchanged —
//! callers receive a `String` and are unaware that streaming happened.
//!
//! Benefits:
//! - No HTTP-level timeout on either side (OpenClaw or Calciforge)
//! - First-byte timeout of 30s is applied server-side in OpenClaw
//! - Calciforge waits until the SSE stream terminates (`data: [DONE]`)

use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::Mutex;

use crate::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::{AdapterError, AgentAdapter, DispatchContext, RuntimeStatus};

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
    /// Temperature for sampling (0.0-2.0). Default 1.0 for GPT-5 compatibility.
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ChatMessage {
    role: String,
    content: String,
}

/// Delta in a streaming chunk.
#[derive(Debug, Deserialize)]
struct ChunkDelta {
    #[serde(default)]
    content: Option<String>,
}

/// A single choice in a streaming chunk.
#[derive(Debug, Deserialize)]
struct ChunkChoice {
    delta: ChunkDelta,
    #[serde(default, rename = "finish_reason")]
    _finish_reason: Option<String>,
}

/// OpenAI-compatible streaming chunk.
#[derive(Debug, Deserialize)]
struct ChatCompletionChunk {
    choices: Vec<ChunkChoice>,
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// Default model name when none is configured.
const DEFAULT_MODEL: &str = "openclaw:main";
/// Default request timeout (ms). Only used as a field; not applied to requests.
const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// OpenAI-compatible HTTP adapter for OpenClaw agents.
///
/// # ⚠️ Native Command Limitation (v3 TODO)
///
/// This adapter dispatches via `/v1/chat/completions` (the LLM path).
/// OpenClaw native commands (`/status`, `/model`, `!approve`, etc.) are
/// **NOT intercepted here** — they are forwarded verbatim to the LLM and
/// processed as ordinary chat messages rather than handled natively.
///
/// For native command support, use `OpenClawNativeAdapter`, which dispatches
/// through OpenClaw's hooks API instead of the OpenAI-compatible LLM path.
pub struct OpenClawHttpAdapter {
    client: reqwest::Client,
    endpoint: String,
    auth_token: String,
    model: String,
    _timeout: Duration,
    /// Agent ID used to build stable per-sender session keys.
    agent_id: String,
}

impl OpenClawHttpAdapter {
    /// Create a new adapter.
    ///
    /// - `endpoint` — base URL, e.g. `http://10.0.0.20:18789`
    /// - `auth_token` — Bearer token (empty string = unauthenticated)
    /// - `model` — model name (`None` → `"openclaw:main"`)
    /// - `timeout_ms` — stored for reference; no per-request timeout is applied
    /// - `agent_id` — used to build `x-openclaw-session-key` for conversation continuity
    #[cfg(test)]
    pub fn new(
        endpoint: String,
        auth_token: String,
        model: Option<String>,
        timeout_ms: Option<u64>,
    ) -> Self {
        Self::new_with_agent_id(endpoint, auth_token, model, timeout_ms, "openclaw")
    }

    pub fn new_with_agent_id(
        endpoint: String,
        auth_token: String,
        model: Option<String>,
        timeout_ms: Option<u64>,
        agent_id: &str,
    ) -> Self {
        let timeout = Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
        let model = model.unwrap_or_else(|| DEFAULT_MODEL.to_string());
        let client = reqwest::Client::builder()
            // No request timeout — LLM calls can take arbitrarily long.
            // OpenClaw applies a 30s first-byte timeout on the streaming side.
            // connect_timeout guards TCP hangs.
            .connect_timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client");
        Self {
            client,
            endpoint,
            auth_token,
            model,
            _timeout: timeout,
            agent_id: agent_id.to_string(),
        }
    }
}

#[async_trait]
impl AgentAdapter for OpenClawHttpAdapter {
    async fn dispatch(&self, msg: &str) -> Result<String, AdapterError> {
        self.dispatch_with_context(DispatchContext::message_only(msg))
            .await
    }

    async fn dispatch_with_context(
        &self,
        ctx: DispatchContext<'_>,
    ) -> Result<String, AdapterError> {
        let msg = ctx.message;
        let url = format!(
            "{}/v1/chat/completions",
            self.endpoint.trim_end_matches('/')
        );

        let body = ChatRequest {
            model: ctx
                .model_override
                .unwrap_or(self.model.as_str())
                .to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: msg.to_string(),
            }],
            stream: true,
            temperature: Some(1.0), // GPT-5 requires temperature=1.0
        };

        info!(endpoint = %url, model = %self.model, "openclaw-http dispatch (streaming)");
        debug!(msg = %msg, "outbound message");

        // Build a stable session key so OpenClaw maintains per-sender conversation
        // history across Calciforge-routed messages.  Without this, each call creates
        // a fresh OpenClaw session and the agent loses all prior context.
        // Key format: "calciforge-{agent_id}-{sender}" e.g. "calciforge-librarian-brian"
        let session_key = ctx
            .sender
            .map(|s| format!("calciforge-{}-{}", self.agent_id, s));

        let mut req = self
            .client
            .post(&url)
            .bearer_auth(&self.auth_token)
            .header("Accept", "text/event-stream");

        if let Some(ref key) = session_key {
            req = req.header("x-openclaw-session-key", key);
        }

        let resp = req.json(&body).send().await.map_err(|e| {
            if e.is_timeout() {
                AdapterError::Timeout
            } else {
                AdapterError::Unavailable(e.to_string())
            }
        })?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            warn!(status = %status, body = %body_text, "agent returned error status");
            return Err(AdapterError::Protocol(format!(
                "HTTP {}: {}",
                status, body_text
            )));
        }

        // ── Consume SSE stream ──────────────────────────────────────────
        // Collect content deltas until [DONE] is received.
        let mut accumulated = String::new();
        let mut bytes_stream = resp.bytes_stream();

        // Buffer for incomplete SSE lines
        let mut line_buf = String::new();

        use futures_util::StreamExt as _;

        loop {
            match bytes_stream.next().await {
                None => break, // Stream ended
                Some(Err(e)) => {
                    // Network error mid-stream — return what we have so far
                    // (or error if nothing received)
                    warn!("SSE stream error: {e}");
                    if accumulated.is_empty() {
                        return Err(AdapterError::Protocol(format!("SSE stream error: {e}")));
                    }
                    break;
                }
                Some(Ok(bytes)) => {
                    let text = String::from_utf8_lossy(&bytes);
                    line_buf.push_str(&text);

                    // Process complete lines
                    while let Some(pos) = line_buf.find('\n') {
                        let line = line_buf[..pos].trim_end_matches('\r').to_string();
                        line_buf = line_buf[pos + 1..].to_string();

                        if let Some(data) = line.strip_prefix("data: ") {
                            let data = data.trim();

                            if data == "[DONE]" {
                                // Stream complete — we're done
                                info!(
                                    "openclaw-http: streaming complete, {} chars received",
                                    accumulated.len()
                                );
                                debug!(response = %accumulated, "agent response");
                                return Ok(if accumulated.is_empty() {
                                    String::new()
                                } else {
                                    accumulated
                                });
                            }

                            // Parse the JSON chunk
                            match serde_json::from_str::<ChatCompletionChunk>(data) {
                                Ok(chunk) => {
                                    for choice in chunk.choices {
                                        if let Some(content) = choice.delta.content {
                                            accumulated.push_str(&content);
                                        }
                                    }
                                }
                                Err(e) => {
                                    debug!("SSE parse error (non-fatal): {e} for data: {data}");
                                    // Non-fatal: skip unparseable lines (comments, keep-alive, etc.)
                                }
                            }
                        }
                        // Lines not starting with "data: " (e.g. "event:", "id:", empty) are ignored
                    }
                }
            }
        }

        info!(
            "openclaw-http: stream ended, {} chars received",
            accumulated.len()
        );
        debug!(response = %accumulated, "agent response");
        Ok(if accumulated.is_empty() {
            String::new()
        } else {
            accumulated
        })
    }

    fn kind(&self) -> &'static str {
        "openclaw-http"
    }
}

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
/// Unlike `openclaw-http` which uses the OpenAI-compat `/v1/chat/completions`
/// shim, this adapter calls ZeroClaw's native webhook endpoint which runs the full
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

    // Helper: build adapter pointing at a local test URL
    fn make_adapter(port: u16, token: &str) -> OpenClawHttpAdapter {
        OpenClawHttpAdapter::new(
            format!("http://127.0.0.1:{}", port),
            token.to_string(),
            Some("test-model".to_string()),
            Some(2000),
        )
    }

    #[test]
    fn test_url_construction_no_trailing_slash() {
        let endpoint = "http://10.0.0.20:18789";
        let url = format!("{}/v1/chat/completions", endpoint.trim_end_matches('/'));
        assert_eq!(url, "http://10.0.0.20:18789/v1/chat/completions");
    }

    #[test]
    fn test_url_construction_with_trailing_slash() {
        let endpoint = "http://10.0.0.20:18789/";
        let url = format!("{}/v1/chat/completions", endpoint.trim_end_matches('/'));
        assert_eq!(url, "http://10.0.0.20:18789/v1/chat/completions");
    }

    #[test]
    fn test_default_model_applied() {
        let adapter = OpenClawHttpAdapter::new(
            "http://localhost".to_string(),
            "tok".to_string(),
            None,
            None,
        );
        assert_eq!(adapter.model, DEFAULT_MODEL);
    }

    #[test]
    fn test_chat_request_serialization() {
        let req = ChatRequest {
            model: "openclaw:main".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "hello world".to_string(),
            }],
            stream: true,
            temperature: Some(1.0),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "openclaw:main");
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "hello world");
        assert_eq!(json["stream"], true);
    }

    #[test]
    fn test_chunk_delta_deserialization() {
        let raw = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1234567890,"model":"test","choices":[{"index":0,"delta":{"content":"hello"},"finish_reason":null}]}"#;
        let chunk: ChatCompletionChunk = serde_json::from_str(raw).unwrap();
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("hello"));
    }

    #[test]
    fn test_chunk_delta_stop_deserialization() {
        let raw = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1234567890,"model":"test","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
        let chunk: ChatCompletionChunk = serde_json::from_str(raw).unwrap();
        assert!(chunk.choices[0].delta.content.is_none());
        assert_eq!(chunk.choices[0]._finish_reason.as_deref(), Some("stop"));
    }

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

    #[tokio::test]
    async fn test_dispatch_to_unreachable_returns_unavailable() {
        let adapter = make_adapter(19091, "tok");
        let result = adapter.dispatch("ping").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            AdapterError::Unavailable(_) => {}
            other => panic!("expected Unavailable, got {:?}", other),
        }
    }
}
