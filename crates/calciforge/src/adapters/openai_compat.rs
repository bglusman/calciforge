//! Generic OpenAI-compatible chat-completions adapter.
//!
//! This adapter is for model-gateway style endpoints that expose
//! `POST /v1/chat/completions`. It is intentionally separate from
//! `openclaw-channel`: OpenClaw agents need the native channel/plugin path for
//! slash commands and agent runtime semantics, while model-gateway endpoints
//! are plain LLM chat targets.

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::{AdapterError, AgentAdapter, DispatchContext};

const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// Adapter for OpenAI-compatible `/v1/chat/completions` endpoints.
pub struct OpenAiCompatAdapter {
    client: reqwest::Client,
    endpoint: String,
    auth_token: String,
    model: Option<String>,
}

impl OpenAiCompatAdapter {
    pub fn new(
        endpoint: String,
        auth_token: String,
        model: Option<String>,
        timeout_ms: Option<u64>,
    ) -> Self {
        let builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_millis(
                timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS),
            ));
        let client = builder.build().expect("reqwest client");

        Self {
            client,
            endpoint,
            auth_token,
            model,
        }
    }

    fn chat_completions_url(&self) -> String {
        let base = self.endpoint.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{base}/chat/completions")
        } else {
            format!("{base}/v1/chat/completions")
        }
    }

    fn requested_model<'a>(
        &'a self,
        ctx: &'a DispatchContext<'_>,
    ) -> Result<&'a str, AdapterError> {
        ctx.model_override.or(self.model.as_deref()).ok_or_else(|| {
            AdapterError::Protocol(
                "openai-compat requires a configured model or active model override".to_string(),
            )
        })
    }
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
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

#[async_trait]
impl AgentAdapter for OpenAiCompatAdapter {
    async fn dispatch(&self, msg: &str) -> Result<String, AdapterError> {
        self.dispatch_with_context(DispatchContext::message_only(msg))
            .await
    }

    async fn dispatch_with_context(
        &self,
        ctx: DispatchContext<'_>,
    ) -> Result<String, AdapterError> {
        let requested_model = self.requested_model(&ctx)?;
        let url = self.chat_completions_url();
        let body = ChatRequest {
            model: requested_model.to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: ctx.message.to_string(),
            }],
            stream: false,
            temperature: Some(1.0),
        };

        info!(
            endpoint = %url,
            configured_model = ?self.model,
            requested_model = %requested_model,
            "openai-compat dispatch"
        );

        let mut req = self.client.post(&url);
        if !self.auth_token.is_empty() {
            req = req.bearer_auth(&self.auth_token);
        }

        let resp = req.json(&body).send().await.map_err(|e| {
            if e.is_timeout() {
                AdapterError::Timeout
            } else {
                AdapterError::Unavailable(e.to_string())
            }
        })?;

        let status = resp.status();
        let body_text = resp.text().await.map_err(|e| {
            if e.is_timeout() {
                AdapterError::Timeout
            } else {
                AdapterError::Unavailable(e.to_string())
            }
        })?;

        if !status.is_success() {
            warn!(status = %status, body = %body_text, "openai-compat error response");
            return Err(AdapterError::Protocol(format!(
                "HTTP {status}: {body_text}"
            )));
        }

        let parsed: ChatResponse = serde_json::from_str(&body_text)
            .map_err(|e| AdapterError::Protocol(format!("openai-compat JSON parse error: {e}")))?;

        if let Some(err) = parsed.error {
            return Err(AdapterError::Protocol(format!(
                "openai-compat API error: {}",
                err.message
            )));
        }

        parsed
            .choices
            .into_iter()
            .find_map(|choice| choice.message.content)
            .ok_or_else(|| {
                AdapterError::Protocol(
                    "openai-compat response did not include choices[0].message.content".to_string(),
                )
            })
    }

    fn kind(&self) -> &'static str {
        "openai-compat"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::Json;
    use axum::routing::post;
    use axum::Router;
    use serde_json::{json, Value};
    use tokio::net::TcpListener;

    async fn spawn_chat_server() -> (String, tokio::task::JoinHandle<()>) {
        async fn chat(Json(payload): Json<Value>) -> Json<Value> {
            let model = payload["model"].as_str().unwrap_or("<missing>");
            let message = payload["messages"][0]["content"]
                .as_str()
                .unwrap_or("<missing>");
            Json(json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": format!("model={model}; message={message}")
                    }
                }]
            }))
        }

        let app = Router::new().route("/v1/chat/completions", post(chat));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), handle)
    }

    #[test]
    fn chat_completions_url_accepts_base_or_v1_endpoint() {
        let base = OpenAiCompatAdapter::new(
            "http://localhost:8083".to_string(),
            String::new(),
            Some("local".to_string()),
            None,
        );
        assert_eq!(
            base.chat_completions_url(),
            "http://localhost:8083/v1/chat/completions"
        );

        let v1 = OpenAiCompatAdapter::new(
            "http://localhost:8083/v1/".to_string(),
            String::new(),
            Some("local".to_string()),
            None,
        );
        assert_eq!(
            v1.chat_completions_url(),
            "http://localhost:8083/v1/chat/completions"
        );
    }

    #[tokio::test]
    async fn dispatch_uses_configured_model() {
        let (endpoint, _server) = spawn_chat_server().await;
        let adapter = OpenAiCompatAdapter::new(
            endpoint,
            String::new(),
            Some("local-kimi-gpt55".to_string()),
            None,
        );

        let response = adapter.dispatch("hello").await.unwrap();
        assert_eq!(response, "model=local-kimi-gpt55; message=hello");
    }

    #[tokio::test]
    async fn model_override_wins() {
        let (endpoint, _server) = spawn_chat_server().await;
        let adapter = OpenAiCompatAdapter::new(
            endpoint,
            String::new(),
            Some("configured".to_string()),
            None,
        );

        let response = adapter
            .dispatch_with_context(DispatchContext {
                message: "hello",
                sender: Some("brian"),
                model_override: Some("override"),
            })
            .await
            .unwrap();
        assert_eq!(response, "model=override; message=hello");
    }

    #[tokio::test]
    async fn missing_model_is_protocol_error() {
        let (endpoint, _server) = spawn_chat_server().await;
        let adapter = OpenAiCompatAdapter::new(endpoint, String::new(), None, None);

        let err = adapter.dispatch("hello").await.unwrap_err().to_string();
        assert!(err.contains("configured model"), "got: {err}");
    }
}
