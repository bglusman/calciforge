//! OpenClawNativeAdapter — dispatches via OpenClaw's `/hooks/agent` native pipeline.
//!
//! ## Why this exists
//!
//! OpenAI-compatible `/v1/chat/completions` integrations bypass the native
//! agent pipeline entirely. Two bugs follow:
//!
//! 1. **No session continuity** — every request is stateless from OpenClaw's
//!    perspective; the agent has no memory of prior turns.
//! 2. **Native commands broken** — `/status`, `/model`, `!approve`, `!deny` etc.
//!    are forwarded to the LLM as plain text rather than dispatched through
//!    OpenClaw's command pipeline.
//!
//! ## Current status
//!
//! Current OpenClaw releases may acknowledge `/hooks/agent` with only a `runId`
//! and complete asynchronously. Calciforge therefore rejects `openclaw-native`
//! in config validation, doctor, and the public adapter factory for chat use.
//! Keep this module as a low-level test/automation surface; use
//! `openclaw-channel` for real chat routing.
//!
//! ## OpenClaw gateway requirements
//!
//! The `/hooks/agent` endpoint requires `hooks.enabled = true` and a
//! `hooks.token` in the OpenClaw config.  The adapter reads the token from the
//! per-agent `api_key` / `auth_token` config field (same as the HTTP adapter).
//!
//! **Session key policy:**  `/hooks/agent` by default disables caller-supplied
//! `sessionKey` values (`hooks.allowRequestSessionKey = false`).  To enable
//! native session continuity, set in your OpenClaw config:
//!
//! ```json5
//! {
//!   hooks: {
//!     allowRequestSessionKey: true,
//!     allowedSessionKeyPrefixes: ["calciforge:"],
//!   }
//! }
//! ```
//!
//! If the gateway rejects the `sessionKey`, this adapter falls back to
//! stateless delivery (one-shot, no continuity) while logging a warning.
//!
//! Do not configure `[[agents]] kind = "openclaw-native"` in Calciforge chat
//! configs. The validator rejects it so deployments do not accidentally use an
//! async hook acknowledgement as a chat reply path.

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::{AdapterError, AgentAdapter, DispatchContext};

// ---------------------------------------------------------------------------
// Wire types — /hooks/agent request and response
// ---------------------------------------------------------------------------

/// Request body for `POST /hooks/agent`.
#[derive(Debug, Serialize)]
struct HooksAgentRequest<'a> {
    /// The user message to deliver to the agent.
    message: &'a str,
    /// Human-readable hook name used in session summaries.
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    /// Session key for conversation continuity.
    ///
    /// Requires `hooks.allowRequestSessionKey = true` on the OpenClaw side.
    /// When omitted (or rejected), each request gets a fresh session.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "sessionKey")]
    session_key: Option<String>,
    /// Whether to deliver the reply to the agent's last messaging channel.
    ///
    /// Set to `false` — Calciforge receives the response as the HTTP reply and
    /// routes it to the originating chat channel itself.
    deliver: bool,
    /// Optional agent id to route the hook to a specific OpenClaw agent.
    ///
    /// Maps to `x-openclaw-agent-id` semantics at the hook layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "agentId")]
    agent_id: Option<&'a str>,
}

/// Successful response body from `POST /hooks/agent`.
///
/// OpenClaw acknowledges the hook synchronously; the agent loop may still be
/// running when the 200 arrives.  Calciforge needs a separate poll or inline
/// response; see `deliver: false` flow.
///
/// Some OpenClaw releases acknowledge the hook with a run id and execute it
/// asynchronously instead of returning an inline response.
#[derive(Debug, Deserialize)]
struct HooksAgentResponse {
    /// Agent response text.  Present when `deliver = false` and the agent
    /// loop completed synchronously.
    #[serde(default)]
    response: Option<String>,
    /// Indicates whether the hook was accepted and queued / run.
    #[serde(default, rename = "ok")]
    _ok: Option<bool>,
    /// Error message from OpenClaw (non-fatal protocol error).
    #[serde(default)]
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// Low-level OpenClaw `/hooks/agent` adapter.
///
/// Do not use this for Calciforge chat routing unless OpenClaw grows a
/// synchronous reply contract or Calciforge adds polling for accepted hook runs.
pub struct OpenClawNativeAdapter {
    client: reqwest::Client,
    /// Base URL, e.g. `http://10.0.0.20:18789`.
    endpoint: String,
    /// Hooks token (`hooks.token` in OpenClaw config).
    hooks_token: String,
    /// Agent id used in the `agentId` field and to build session keys.
    agent_id: String,
    /// Hooks path (default `/hooks`; configurable for non-standard deployments).
    hooks_path: String,
    #[allow(dead_code)]
    timeout: Duration,
}

impl OpenClawNativeAdapter {
    /// Create a new native adapter.
    ///
    /// - `endpoint`    — base URL, e.g. `http://10.0.0.20:18789`
    /// - `hooks_token` — `hooks.token` from OpenClaw config (NOT the gateway token)
    /// - `agent_id`    — agent id used for `agentId` routing and session key derivation
    /// - `hooks_path`  — path prefix (default `/hooks`; use when `hooks.path` is non-default)
    /// - `timeout_ms`  — request timeout (default 120 s)
    pub fn new(
        endpoint: String,
        hooks_token: String,
        agent_id: String,
        hooks_path: Option<String>,
        timeout_ms: Option<u64>,
    ) -> Self {
        let timeout = Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
        let hooks_path = hooks_path.unwrap_or_else(|| "/hooks".to_string());
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(timeout)
            .build()
            .expect("reqwest client");
        Self {
            client,
            endpoint,
            hooks_token,
            agent_id,
            hooks_path,
            timeout,
        }
    }

    /// Build the full URL for `POST /hooks/agent`.
    fn hooks_agent_url(&self) -> String {
        format!(
            "{}{}/agent",
            self.endpoint.trim_end_matches('/'),
            self.hooks_path.trim_end_matches('/')
        )
    }

    /// Derive a stable session key for a given sender.
    ///
    /// Format: `"calciforge:{agent_id}:{sender}"` — e.g. `"calciforge:librarian:brian"`.
    ///
    /// The `calciforge:` prefix should be listed in `hooks.allowedSessionKeyPrefixes`
    /// on the OpenClaw side to allow caller-supplied session keys.
    pub fn session_key_for(&self, sender: &str) -> String {
        format!("calciforge:{}:{}", self.agent_id, sender)
    }

    /// Derive a session key when no sender is known.
    ///
    /// Uses a shared key scoped to the agent, not the sender.
    pub fn default_session_key(&self) -> String {
        format!("calciforge:{}", self.agent_id)
    }
}

#[async_trait]
impl AgentAdapter for OpenClawNativeAdapter {
    async fn dispatch(&self, msg: &str) -> Result<String, AdapterError> {
        self.dispatch_with_context(DispatchContext::message_only(msg))
            .await
    }

    async fn dispatch_with_context(
        &self,
        ctx: DispatchContext<'_>,
    ) -> Result<String, AdapterError> {
        let url = self.hooks_agent_url();

        // Build a stable session key from sender identity.
        // Requires `hooks.allowRequestSessionKey = true` on the OpenClaw side.
        let session_key = ctx
            .sender
            .map(|s| self.session_key_for(s))
            .or_else(|| Some(self.default_session_key()));

        info!(
            endpoint = %url,
            agent_id = %self.agent_id,
            sender = ?ctx.sender,
            session_key = ?session_key,
            "openclaw-native dispatch"
        );
        debug!(msg = %ctx.message, "outbound message");

        let body = HooksAgentRequest {
            message: ctx.message,
            name: Some("Calciforge"),
            session_key,
            // deliver = false: OpenClaw runs the loop synchronously and returns
            // the response inline.  Calciforge then routes it to the channel.
            deliver: false,
            agent_id: Some(&self.agent_id),
        };

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.hooks_token)
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
            let body_text = resp.text().await.unwrap_or_default();

            // 400 can indicate that `allowRequestSessionKey` is disabled.
            // Warn clearly so operators can configure OpenClaw accordingly.
            if status == reqwest::StatusCode::BAD_REQUEST && body_text.contains("sessionKey") {
                warn!(
                    "openclaw-native: session key rejected by OpenClaw. \
                    Set hooks.allowRequestSessionKey=true and \
                    hooks.allowedSessionKeyPrefixes=[\"calciforge:\"] \
                    in your OpenClaw config for session continuity."
                );
            }

            return Err(AdapterError::Protocol(format!(
                "openclaw-native HTTP {}: {}",
                status, body_text
            )));
        }

        let hooks_resp: HooksAgentResponse = resp.json().await.map_err(|e| {
            AdapterError::Protocol(format!("openclaw-native JSON parse error: {e}"))
        })?;

        // Surface explicit error from OpenClaw
        if let Some(err) = hooks_resp.error {
            return Err(AdapterError::Protocol(format!(
                "openclaw-native agent error: {err}"
            )));
        }

        let Some(reply) = hooks_resp.response.filter(|s| !s.is_empty()) else {
            return Err(AdapterError::Protocol(
                "openclaw-native accepted the hook run but did not return an inline response; use openclaw-channel or a Gateway RPC adapter for synchronous replies".to_string(),
            ));
        };

        info!(len = reply.len(), "openclaw-native: response received");
        debug!(response = %reply, "agent response");

        Ok(reply)
    }

    fn kind(&self) -> &'static str {
        "openclaw-native"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_adapter(port: u16) -> OpenClawNativeAdapter {
        OpenClawNativeAdapter::new(
            format!("http://127.0.0.1:{}", port),
            "test-hooks-token".to_string(),
            "librarian".to_string(),
            None,
            Some(2000),
        )
    }

    #[test]
    fn test_hooks_agent_url_no_trailing_slash() {
        let a = OpenClawNativeAdapter::new(
            "http://10.0.0.20:18789".to_string(),
            "tok".to_string(),
            "main".to_string(),
            None,
            None,
        );
        assert_eq!(a.hooks_agent_url(), "http://10.0.0.20:18789/hooks/agent");
    }

    #[test]
    fn test_hooks_agent_url_with_trailing_slash() {
        let a = OpenClawNativeAdapter::new(
            "http://10.0.0.20:18789/".to_string(),
            "tok".to_string(),
            "main".to_string(),
            None,
            None,
        );
        assert_eq!(a.hooks_agent_url(), "http://10.0.0.20:18789/hooks/agent");
    }

    #[test]
    fn test_hooks_agent_url_custom_path() {
        let a = OpenClawNativeAdapter::new(
            "http://localhost:18789".to_string(),
            "tok".to_string(),
            "main".to_string(),
            Some("/webhooks".to_string()),
            None,
        );
        assert_eq!(a.hooks_agent_url(), "http://localhost:18789/webhooks/agent");
    }

    #[test]
    fn test_session_key_for_sender() {
        let a = make_adapter(19100);
        assert_eq!(a.session_key_for("brian"), "calciforge:librarian:brian");
        assert_eq!(a.session_key_for("renee"), "calciforge:librarian:renee");
    }

    #[test]
    fn test_default_session_key() {
        let a = make_adapter(19100);
        assert_eq!(a.default_session_key(), "calciforge:librarian");
    }

    #[test]
    fn test_session_key_format_uses_agent_id() {
        let a = OpenClawNativeAdapter::new(
            "http://localhost".to_string(),
            "tok".to_string(),
            "custodian".to_string(),
            None,
            None,
        );
        assert_eq!(a.session_key_for("alice"), "calciforge:custodian:alice");
    }

    #[test]
    fn test_hooks_agent_request_serialization() {
        let req = HooksAgentRequest {
            message: "hello",
            name: Some("Calciforge"),
            session_key: Some("calciforge:librarian:brian".to_string()),
            deliver: false,
            agent_id: Some("librarian"),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["message"], "hello");
        assert_eq!(json["name"], "Calciforge");
        assert_eq!(json["sessionKey"], "calciforge:librarian:brian");
        assert_eq!(json["deliver"], false);
        assert_eq!(json["agentId"], "librarian");
    }

    #[test]
    fn test_hooks_agent_request_no_session_key() {
        let req = HooksAgentRequest {
            message: "ping",
            name: None,
            session_key: None,
            deliver: false,
            agent_id: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        // sessionKey and name and agentId should be absent
        assert!(json.get("sessionKey").is_none());
        assert!(json.get("name").is_none());
        assert!(json.get("agentId").is_none());
    }

    #[tokio::test]
    async fn test_dispatch_to_unreachable_returns_unavailable() {
        let a = make_adapter(19201);
        let result = a.dispatch("ping").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            AdapterError::Unavailable(_) => {}
            other => panic!("expected Unavailable, got {:?}", other),
        }
    }

    /// Verify that the adapter always includes a session_key in requests
    /// (either sender-specific or default).
    #[tokio::test]
    async fn test_openclaw_native_adapter_passes_session_key() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        // Canned JSON response from /hooks/agent
        let json_body = r#"{"ok":true,"response":"native reply"}"#;
        let http_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            json_body.len(),
            json_body
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let captured = std::sync::Arc::new(tokio::sync::Mutex::new(String::new()));
        let captured_srv = captured.clone();

        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                *captured_srv.lock().await = String::from_utf8_lossy(&buf[..n]).to_string();
                let _ = stream.write_all(http_response.as_bytes()).await;
                let _ = stream.flush().await;
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        });

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let a = OpenClawNativeAdapter::new(
            format!("http://127.0.0.1:{}", port),
            "test-token".to_string(),
            "librarian".to_string(),
            None,
            Some(2000),
        );

        use crate::adapters::DispatchContext;
        let ctx = DispatchContext {
            message: "hello",
            sender: Some("brian"),
            model_override: None,
            session: None,
            channel: None,
        };
        let result = a.dispatch_with_context(ctx).await;

        let req_text = captured.lock().await.clone();

        // 1. Request must contain the session key
        assert!(
            req_text.contains("calciforge:librarian:brian"),
            "expected session key 'calciforge:librarian:brian' in request body, got:\n{}",
            req_text
        );
        // 2. Request must contain deliver:false
        assert!(
            req_text.contains("\"deliver\":false"),
            "expected deliver:false in request body"
        );
        // 3. Dispatch must succeed
        assert!(result.is_ok(), "dispatch failed: {:?}", result);
        assert_eq!(result.unwrap(), "native reply");
    }

    #[tokio::test]
    async fn test_openclaw_native_rejects_async_only_ack() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let json_body = r#"{"ok":true,"runId":"accepted-but-async"}"#;
        let http_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            json_body.len(),
            json_body
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let _ = stream.read(&mut buf).await.unwrap_or(0);
                let _ = stream.write_all(http_response.as_bytes()).await;
                let _ = stream.flush().await;
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        });

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let a = OpenClawNativeAdapter::new(
            format!("http://127.0.0.1:{}", port),
            "test-token".to_string(),
            "librarian".to_string(),
            None,
            Some(2000),
        );

        let result = a.dispatch("hello").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            AdapterError::Protocol(msg) => {
                assert!(msg.contains("did not return an inline response"), "{msg}");
            }
            other => panic!("expected Protocol, got {other:?}"),
        }
    }

    /// Verify that the session key is consistent across multiple calls with the same sender.
    #[tokio::test]
    async fn test_openclaw_native_maintains_session_across_turns() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let json_body = r#"{"ok":true,"response":"turn reply"}"#;
        let make_response = || {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                json_body.len(),
                json_body
            )
        };

        // Collect session keys from multiple requests
        let session_keys: std::sync::Arc<tokio::sync::Mutex<Vec<String>>> =
            std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let keys_srv = session_keys.clone();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let resp1 = make_response();
        let resp2 = make_response();

        tokio::spawn(async move {
            // Handle two sequential connections
            for response in [resp1, resp2] {
                if let Ok((mut stream, _)) = listener.accept().await {
                    let mut buf = vec![0u8; 8192];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    let req_text = String::from_utf8_lossy(&buf[..n]).to_string();

                    // Extract the sessionKey value from the JSON body
                    if let Some(start) = req_text.find("\"sessionKey\":\"") {
                        let rest = &req_text[start + 14..];
                        if let Some(end) = rest.find('"') {
                            let key = rest[..end].to_string();
                            keys_srv.lock().await.push(key);
                        }
                    }

                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.flush().await;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        });

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let a = OpenClawNativeAdapter::new(
            format!("http://127.0.0.1:{}", port),
            "test-token".to_string(),
            "librarian".to_string(),
            None,
            Some(2000),
        );

        use crate::adapters::DispatchContext;

        // Dispatch two messages from the same sender
        for msg in ["first message", "second message"] {
            let ctx = DispatchContext {
                message: msg,
                sender: Some("brian"),
                model_override: None,
                session: None,
                channel: None,
            };
            let _ = a.dispatch_with_context(ctx).await;
        }

        let keys = session_keys.lock().await.clone();

        // Both requests must have sent exactly the same session key
        assert_eq!(
            keys.len(),
            2,
            "expected 2 session key captures, got: {:?}",
            keys
        );
        assert_eq!(
            keys[0], keys[1],
            "session key must be stable across turns: {:?}",
            keys
        );
        assert_eq!(
            keys[0], "calciforge:librarian:brian",
            "unexpected session key format: {}",
            keys[0]
        );
    }

    /// Verify that sender identity is included in the request.
    #[tokio::test]
    async fn test_openclaw_native_forwards_sender_identity() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let json_body = r#"{"ok":true,"response":"ok"}"#;
        let http_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            json_body.len(),
            json_body
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let captured = std::sync::Arc::new(tokio::sync::Mutex::new(String::new()));
        let captured_srv = captured.clone();

        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                *captured_srv.lock().await = String::from_utf8_lossy(&buf[..n]).to_string();
                let _ = stream.write_all(http_response.as_bytes()).await;
                let _ = stream.flush().await;
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        });

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let a = OpenClawNativeAdapter::new(
            format!("http://127.0.0.1:{}", port),
            "test-token".to_string(),
            "librarian".to_string(),
            None,
            Some(2000),
        );

        use crate::adapters::DispatchContext;
        let ctx = DispatchContext {
            message: "status check",
            sender: Some("renee"),
            model_override: None,
            session: None,
            channel: None,
        };
        let _ = a.dispatch_with_context(ctx).await;

        let req_text = captured.lock().await.clone();

        // The session key encodes the sender identity
        assert!(
            req_text.contains("calciforge:librarian:renee"),
            "sender identity 'renee' not found in request (via session key), got:\n{}",
            req_text
        );
        // agentId is forwarded
        assert!(
            req_text.contains("\"agentId\":\"librarian\""),
            "agentId not found in request, got:\n{}",
            req_text
        );
    }
}
