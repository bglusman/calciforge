//! Hermes Agent adapter — connects to a running hermes-agent instance via
//! its OpenAI-compatible API server platform.
//!
//! Hermes exposes `POST /v1/chat/completions` on port 8642 (default) which
//! invokes the full agent pipeline (tools, memory, skills, multi-step reasoning).
//! Session continuity is maintained via the `X-Hermes-Session-Id` header,
//! keyed on Calciforge's selected session when present, or the sender identity
//! as the stable default thread.

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::{AdapterError, AgentAdapter, DispatchContext};

const DEFAULT_TIMEOUT_MS: u64 = 600_000; // 10 minutes — agent may run many tool iterations

pub struct HermesAdapter {
    client: reqwest::Client,
    endpoint: String,
    auth_token: String,
    model: String,
}

impl HermesAdapter {
    pub fn new(
        endpoint: String,
        auth_token: String,
        model: Option<String>,
        timeout_ms: Option<u64>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_millis(
                timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS),
            ))
            .build()
            .expect("reqwest client");

        Self {
            client,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            auth_token,
            model: model.unwrap_or_else(|| "hermes-agent".to_string()),
        }
    }

    fn chat_completions_url(&self) -> String {
        let base = &self.endpoint;
        if base.ends_with("/v1") {
            format!("{base}/chat/completions")
        } else {
            format!("{base}/v1/chat/completions")
        }
    }

    async fn send_message(
        &self,
        message: &str,
        sender: Option<&str>,
        session: Option<&str>,
    ) -> Result<String, AdapterError> {
        let url = self.chat_completions_url();

        let body = ChatRequest {
            model: self.model.clone(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: message.to_string(),
            }],
            stream: false,
        };

        let mut req = self.client.post(&url).json(&body);

        if !self.auth_token.is_empty() {
            req = req.bearer_auth(&self.auth_token);
        }

        // Session continuity: prefer Calciforge's selected session, falling
        // back to sender identity so each user has a stable default thread.
        if let Some(session_id) = session_header_value(sender, session) {
            req = req.header("X-Hermes-Session-Id", session_id);
        }

        info!(endpoint = %url, sender = ?sender, session = ?session, "hermes dispatch");

        let resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                AdapterError::Timeout
            } else {
                AdapterError::Unavailable(format!("Hermes API failed: {e}"))
            }
        })?;

        let status = resp.status();
        let body_text = resp.text().await.map_err(|e| {
            if e.is_timeout() {
                AdapterError::Timeout
            } else {
                AdapterError::Unavailable(format!("Hermes response read failed: {e}"))
            }
        })?;

        if !status.is_success() {
            warn!(status = %status, body = %body_text, "hermes error response");
            return Err(AdapterError::Protocol(format!(
                "Hermes HTTP {status}: {body_text}"
            )));
        }

        let parsed: ChatResponse = serde_json::from_str(&body_text)
            .map_err(|e| AdapterError::Protocol(format!("Hermes JSON parse error: {e}")))?;

        if let Some(err) = parsed.error {
            return Err(AdapterError::Protocol(format!(
                "Hermes API error: {}",
                err.message
            )));
        }

        parsed
            .choices
            .into_iter()
            .find_map(|choice| choice.message.content)
            .ok_or_else(|| {
                AdapterError::Protocol(
                    "Hermes response did not include choices[0].message.content".to_string(),
                )
            })
    }
}

#[async_trait]
impl AgentAdapter for HermesAdapter {
    async fn dispatch(&self, msg: &str) -> Result<String, AdapterError> {
        self.send_message(msg, None, None).await
    }

    async fn dispatch_with_context(
        &self,
        ctx: DispatchContext<'_>,
    ) -> Result<String, AdapterError> {
        self.send_message(ctx.message, ctx.sender, ctx.session)
            .await
    }

    fn kind(&self) -> &'static str {
        "hermes"
    }
}

fn session_header_value(sender: Option<&str>, session: Option<&str>) -> Option<String> {
    let raw = session
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| sender.map(str::trim).filter(|value| !value.is_empty()))?;

    let mut safe = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            safe.push(ch);
        } else {
            safe.push('_');
        }
    }
    Some(format!("calciforge-{safe}"))
}

// ── Wire types ──────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<ChatChoice>,
    #[serde(default)]
    error: Option<ApiError>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct ChatChoiceMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    message: String,
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::IntoFuture;

    #[test]
    fn chat_completions_url_without_v1() {
        let adapter = HermesAdapter::new(
            "http://localhost:8642".to_string(),
            "test-key".to_string(),
            None,
            None,
        );
        assert_eq!(
            adapter.chat_completions_url(),
            "http://localhost:8642/v1/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_with_v1() {
        let adapter = HermesAdapter::new(
            "http://localhost:8642/v1".to_string(),
            "test-key".to_string(),
            None,
            None,
        );
        assert_eq!(
            adapter.chat_completions_url(),
            "http://localhost:8642/v1/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_strips_trailing_slash() {
        let adapter = HermesAdapter::new(
            "http://localhost:8642/".to_string(),
            "test-key".to_string(),
            None,
            None,
        );
        assert_eq!(
            adapter.chat_completions_url(),
            "http://localhost:8642/v1/chat/completions"
        );
    }

    #[test]
    fn default_model_is_hermes_agent() {
        let adapter = HermesAdapter::new(
            "http://localhost:8642".to_string(),
            "".to_string(),
            None,
            None,
        );
        assert_eq!(adapter.model, "hermes-agent");
    }

    #[test]
    fn custom_model() {
        let adapter = HermesAdapter::new(
            "http://localhost:8642".to_string(),
            "".to_string(),
            Some("nous/hermes-3".to_string()),
            None,
        );
        assert_eq!(adapter.model, "nous/hermes-3");
    }

    #[test]
    fn session_header_prefers_explicit_session_and_normalizes_value() {
        assert_eq!(
            session_header_value(Some("sender-one"), Some("thread/with spaces")),
            Some("calciforge-thread_with_spaces".to_string())
        );
    }

    #[test]
    fn session_header_falls_back_to_sender() {
        assert_eq!(
            session_header_value(Some("sender-one"), None),
            Some("calciforge-sender-one".to_string())
        );
    }

    #[test]
    fn session_header_falls_back_to_sender_for_blank_session() {
        assert_eq!(
            session_header_value(Some("sender-one"), Some("   ")),
            Some("calciforge-sender-one".to_string())
        );
    }

    #[tokio::test]
    async fn dispatch_with_mock_server() {
        use axum::routing::post;
        use axum::Router;
        use serde_json::json;
        use tokio::net::TcpListener;

        let app = Router::new().route(
            "/v1/chat/completions",
            post(
                |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                    let msg = body["messages"][0]["content"].as_str().unwrap_or("");
                    axum::Json(json!({
                        "choices": [{
                            "message": {
                                "role": "assistant",
                                "content": format!("Hermes says: {msg}")
                            }
                        }]
                    }))
                },
            ),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, app).into_future());

        let adapter = HermesAdapter::new(
            format!("http://{addr}"),
            "".to_string(),
            Some("test-model".to_string()),
            None,
        );

        let result = adapter.dispatch("hello").await.unwrap();
        assert_eq!(result, "Hermes says: hello");
    }

    #[tokio::test]
    async fn dispatch_with_session_header() {
        use axum::extract::Request;
        use axum::routing::post;
        use axum::Router;
        use serde_json::json;
        use std::sync::Arc;
        use tokio::net::TcpListener;
        use tokio::sync::Mutex;

        let captured_session = Arc::new(Mutex::new(String::new()));
        let captured_clone = captured_session.clone();

        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |req: Request| {
                let captured = captured_clone.clone();
                async move {
                    let session = req
                        .headers()
                        .get("X-Hermes-Session-Id")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    *captured.lock().await = session;
                    axum::Json(json!({
                        "choices": [{
                            "message": {"role": "assistant", "content": "ok"}
                        }]
                    }))
                }
            }),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, app).into_future());

        let adapter = HermesAdapter::new(
            format!("http://{addr}"),
            "".to_string(),
            Some("test".to_string()),
            None,
        );

        let ctx = DispatchContext {
            message: "test",
            sender: Some("brian"),
            model_override: None,
            session: Some("project/thread"),
            channel: None,
        };

        adapter.dispatch_with_context(ctx).await.unwrap();
        assert_eq!(*captured_session.lock().await, "calciforge-project_thread");
    }
}
