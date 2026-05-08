//! Unified backend interface for the model gateway
//!
//! Provides the runtime abstraction used by supported model-provider methods.
//! The production root gateway surface is intentionally small: Calciforge's
//! builtin OpenAI-compatible HTTP upstream adapter, Helicone's external HTTP
//! gateway, and a mock backend for tests.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::sync::Arc;

use crate::config::GatewayFailureKind;
use crate::proxy::openai::{ChatCompletionResponse, MessageContent};

// Helicone router (HTTP adapter)
#[cfg(feature = "helicone")]
use super::helicone_router;

/// Errors that can occur in backend operations
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum BackendError {
    #[error("HTTP request failed: {message}")]
    HttpError {
        message: String,
        kind: GatewayFailureKind,
        status: Option<u16>,
    },

    #[error("Invalid response from backend: {0}")]
    InvalidResponse(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),
}

impl BackendError {
    pub fn failure_kind(&self) -> GatewayFailureKind {
        match self {
            Self::HttpError { kind, .. } => *kind,
            Self::InvalidResponse(_) => GatewayFailureKind::InvalidResponse,
            Self::ConfigError(_) => GatewayFailureKind::Misconfigured,
        }
    }

    pub fn transport(message: impl Into<String>, is_timeout: bool) -> Self {
        Self::HttpError {
            message: message.into(),
            kind: if is_timeout {
                GatewayFailureKind::Timeout
            } else {
                GatewayFailureKind::Network
            },
            status: None,
        }
    }

    pub fn http_status_error(status: reqwest::StatusCode, message: impl Into<String>) -> Self {
        Self::HttpError {
            message: message.into(),
            kind: failure_kind_for_status(status.as_u16()),
            status: Some(status.as_u16()),
        }
    }
}

pub fn failure_kind_for_status(status: u16) -> GatewayFailureKind {
    match status {
        400 => GatewayFailureKind::BadRequest,
        401 => GatewayFailureKind::AuthFailed,
        403 => GatewayFailureKind::Forbidden,
        404 => GatewayFailureKind::ModelNotFound,
        408 => GatewayFailureKind::Timeout,
        413 | 422 => GatewayFailureKind::ContextExceeded,
        429 => GatewayFailureKind::RateLimited,
        500 | 502 | 503 | 504 => GatewayFailureKind::ServerError,
        _ if (400..500).contains(&status) => GatewayFailureKind::BadRequest,
        _ if (500..600).contains(&status) => GatewayFailureKind::ServerError,
        _ => GatewayFailureKind::Unknown,
    }
}

/// Unified backend trait for model-gateway providers.
#[async_trait::async_trait]
#[allow(dead_code)]
pub trait SecretsBackend: Send + Sync {
    /// Execute a chat completion request
    async fn chat_completion(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        stream: bool,
        tools: Option<Vec<crate::proxy::openai::ToolDefinition>>,
        tool_choice: Option<crate::proxy::openai::ToolChoice>,
    ) -> Result<ChatCompletionResponse, BackendError>;

    /// Execute a complete OpenAI-compatible chat completion request.
    ///
    /// Implementations should override this when they can preserve request
    /// fields beyond the legacy parameter list, including provider-specific
    /// extension fields captured by `ChatCompletionRequest::extra_body`.
    async fn chat_completion_request(
        &self,
        request: crate::proxy::openai::ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, BackendError> {
        self.chat_completion(
            request.model,
            request.messages,
            request.stream.unwrap_or(false),
            request.tools,
            request.tool_choice,
        )
        .await
    }

    /// List available models
    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError>;

    /// Get backend type for logging/debugging
    fn backend_type(&self) -> BackendType;
}

/// Backend types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendType {
    /// HTTP to an OpenAI-compatible provider.
    Http,
    /// HTTP to Helicone AI Gateway
    Helicone,
    /// Mock backend for testing
    Mock,
}

impl std::fmt::Display for BackendType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendType::Http => write!(f, "http"),
            BackendType::Helicone => write!(f, "helicone"),
            BackendType::Mock => write!(f, "mock"),
        }
    }
}

// Re-export types from openai module for convenience
pub use crate::proxy::openai::{ChatMessage, Choice, Usage};

/// Function call details
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// Model information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: Option<String>,
    pub provider: Option<String>,
    pub capabilities: Vec<String>,
}

/// Backend configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    pub backend_type: BackendType,

    // HTTP backend config
    pub url: Option<String>,
    pub api_key: Option<String>,
    pub timeout_seconds: Option<u64>,
    pub headers: Option<std::collections::HashMap<String, String>>,

    // Helicone backend config
    pub helicone_url: Option<String>,
    pub helicone_api_key: Option<String>,
    pub helicone_router_name: Option<String>,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            backend_type: BackendType::Mock,
            url: Some("http://localhost:8081".to_string()),
            api_key: None,
            timeout_seconds: Some(30),
            headers: None,
            helicone_url: Some("http://localhost:8080".to_string()),
            helicone_api_key: None,
            helicone_router_name: None,
        }
    }
}

/// Factory function to create backend based on config
pub fn create_backend(config: &BackendConfig) -> Result<Arc<dyn SecretsBackend>, BackendError> {
    match config.backend_type {
        BackendType::Http => {
            let url = config.url.clone().ok_or_else(|| {
                BackendError::ConfigError("Missing url for HTTP backend".to_string())
            })?;
            let api_key = config.api_key.clone().unwrap_or_default();
            let timeout = config.timeout_seconds.unwrap_or(30);
            let headers = config.headers.clone();
            Ok(Arc::new(HttpBackend::new(url, api_key, timeout, headers)))
        }
        BackendType::Helicone => create_helicone_backend(config),
        BackendType::Mock => Ok(Arc::new(MockBackend::new())),
    }
}

#[cfg(feature = "helicone")]
fn create_helicone_backend(
    config: &BackendConfig,
) -> Result<Arc<dyn SecretsBackend>, BackendError> {
    let url = config.helicone_url.clone().ok_or_else(|| {
        BackendError::ConfigError("Missing helicone_url for Helicone backend".to_string())
    })?;
    let api_key = config.helicone_api_key.clone().unwrap_or_default();
    let timeout = config.timeout_seconds.unwrap_or(120);
    let router_name = config
        .helicone_router_name
        .clone()
        .unwrap_or_else(|| "helicone".to_string());
    let helicone_config = helicone_router::HeliconeRouterConfig {
        base_url: url,
        api_key,
        timeout_seconds: timeout,
        router_name,
        enable_caching: true,
        cache_ttl_seconds: 300,
        headers: std::collections::HashMap::new(),
        retry: crate::config::GatewayRetryConfig::default(),
    };
    let router = helicone_router::HeliconeRouter::new(helicone_config).map_err(|e| {
        BackendError::ConfigError(format!("Failed to create Helicone router: {}", e))
    })?;
    Ok(Arc::new(router))
}

#[cfg(not(feature = "helicone"))]
fn create_helicone_backend(
    _config: &BackendConfig,
) -> Result<Arc<dyn SecretsBackend>, BackendError> {
    Err(BackendError::ConfigError(
        "Helicone backend selected but calciforge was built without the helicone feature"
            .to_string(),
    ))
}

// Mock backend implementation
pub struct MockBackend {
    responses: std::collections::HashMap<String, String>,
}

impl MockBackend {
    pub fn new() -> Self {
        let mut responses = std::collections::HashMap::new();
        responses.insert("gpt-4".to_string(), "Hello from GPT-4 mock!".to_string());
        responses.insert(
            "claude-3-5-sonnet".to_string(),
            "Hello from Claude mock!".to_string(),
        );
        responses.insert(
            "kimi-free".to_string(),
            "Hello from Kimi Free mock!".to_string(),
        );

        Self { responses }
    }
}

#[async_trait::async_trait]
impl SecretsBackend for MockBackend {
    async fn chat_completion(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        _stream: bool,
        _tools: Option<Vec<crate::proxy::openai::ToolDefinition>>,
        _tool_choice: Option<crate::proxy::openai::ToolChoice>,
    ) -> Result<ChatCompletionResponse, BackendError> {
        // Simple mock response
        let response_text = self
            .responses
            .get(&model)
            .cloned()
            .unwrap_or_else(|| format!("Mock response for model: {}", model));

        let last_message = messages
            .last()
            .and_then(|m| m.content.as_ref().and_then(|c| c.to_text()))
            .unwrap_or_default();

        Ok(ChatCompletionResponse {
            id: format!("mock-{}", uuid::Uuid::new_v4()),
            object: "chat.completion".to_string(),
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            model: model.clone(),
            choices: vec![Choice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(MessageContent::Text(format!(
                        "{} (responding to: {})",
                        response_text, last_message
                    ))),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning: None,
                    reasoning_content: None,
                },
                finish_reason: Some("stop".to_string()),
                logprobs: None,
            }],
            usage: Usage {
                prompt_tokens: messages
                    .iter()
                    .map(|m| {
                        m.content
                            .as_ref()
                            .and_then(|c| c.to_text())
                            .map(|t| t.len() as u32 / 4)
                            .unwrap_or(0)
                    })
                    .sum(),
                completion_tokens: response_text.len() as u32 / 4,
                total_tokens: 0,
            },
            system_fingerprint: None,
        })
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        Ok(vec![
            ModelInfo {
                id: "gpt-4".to_string(),
                name: Some("GPT-4".to_string()),
                provider: Some("openai".to_string()),
                capabilities: vec!["chat".to_string(), "function-calling".to_string()],
            },
            ModelInfo {
                id: "claude-3-5-sonnet".to_string(),
                name: Some("Claude 3.5 Sonnet".to_string()),
                provider: Some("anthropic".to_string()),
                capabilities: vec!["chat".to_string(), "function-calling".to_string()],
            },
            ModelInfo {
                id: "kimi-free".to_string(),
                name: Some("Kimi Free".to_string()),
                provider: Some("kimi".to_string()),
                capabilities: vec!["chat".to_string()],
            },
        ])
    }

    fn backend_type(&self) -> BackendType {
        BackendType::Mock
    }
}

// HTTP backend implementation - calls OpenAI-compatible API endpoints
#[allow(dead_code)]
pub struct HttpBackend {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    timeout_seconds: u64,
    headers: std::collections::HashMap<String, String>,
}

impl HttpBackend {
    pub fn new(
        base_url: String,
        api_key: String,
        timeout_seconds: u64,
        headers: Option<std::collections::HashMap<String, String>>,
    ) -> Self {
        let mut client_builder =
            reqwest::Client::builder().timeout(std::time::Duration::from_secs(timeout_seconds));

        // Add default headers if provided
        if let Some(headers) = &headers {
            let mut header_map = reqwest::header::HeaderMap::new();
            for (key, value) in headers {
                if let Ok(header_name) = reqwest::header::HeaderName::from_bytes(key.as_bytes()) {
                    if let Ok(header_value) = reqwest::header::HeaderValue::from_str(value) {
                        header_map.insert(header_name, header_value);
                    }
                }
            }
            client_builder = client_builder.default_headers(header_map);
        }

        let client = client_builder.build().expect("Failed to build HTTP client");

        Self {
            client,
            base_url,
            api_key,
            timeout_seconds,
            headers: headers.unwrap_or_default(),
        }
    }

    async fn send_chat_completion_request(
        &self,
        mut request: crate::proxy::openai::ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, BackendError> {
        let url = format!("{}/chat/completions", self.base_url);

        // Force non-streaming until this backend grows SSE support.
        request.stream = Some(false);

        let model = request.model.clone();
        let mut request_body = serde_json::to_value(&request).map_err(|e| {
            BackendError::InvalidResponse(format!("Failed to serialize request: {e}"))
        })?;
        apply_kimi_compat(&self.base_url, &model, &mut request_body);

        let mut request_builder = self
            .client
            .post(&url)
            .header("Content-Type", "application/json");

        if !self.api_key.is_empty() {
            request_builder =
                request_builder.header("Authorization", format!("Bearer {}", self.api_key));
        }

        for (key, value) in &self.headers {
            request_builder = request_builder.header(key, value);
        }

        let response = request_builder
            .json(&request_body)
            .send()
            .await
            .map_err(|e| {
                BackendError::transport(format!("Request failed: {}", e), e.is_timeout())
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(BackendError::http_status_error(
                status,
                format!("API error {}: {}", status, error_text),
            ));
        }

        response
            .json()
            .await
            .map_err(|e| BackendError::InvalidResponse(format!("Failed to parse response: {}", e)))
    }
}

fn is_kimi_backend(base_url: &str) -> bool {
    let base_url = base_url.to_ascii_lowercase();
    base_url.contains("api.kimi.com") || base_url.contains("moonshot")
}

fn is_kimi_model(model: &str) -> bool {
    let model = model.trim_start_matches("kimi/");
    model.starts_with("kimi-")
}

fn apply_kimi_compat(base_url: &str, model: &str, request_body: &mut serde_json::Value) {
    if (is_kimi_backend(base_url) || is_kimi_model(model)) && request_body.get("thinking").is_none()
    {
        // Kimi K2.5/K2.6 enable thinking by default. In tool-call conversations
        // the API then requires every prior assistant tool-call message to carry
        // reasoning_content. Many OpenAI-compatible clients do not preserve that
        // provider-specific field. Apply this by backend URL as well as by
        // model name so configured aliases/dispatchers that terminate at Kimi
        // get the same compatibility behavior.
        request_body["thinking"] = serde_json::json!({ "type": "disabled" });
    }
}

#[async_trait::async_trait]
impl SecretsBackend for HttpBackend {
    async fn chat_completion(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        stream: bool,
        tools: Option<Vec<crate::proxy::openai::ToolDefinition>>,
        tool_choice: Option<crate::proxy::openai::ToolChoice>,
    ) -> Result<ChatCompletionResponse, BackendError> {
        self.send_chat_completion_request(crate::proxy::openai::ChatCompletionRequest {
            model,
            messages,
            stream: Some(stream),
            tools,
            tool_choice,
            ..Default::default()
        })
        .await
    }

    async fn chat_completion_request(
        &self,
        request: crate::proxy::openai::ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, BackendError> {
        self.send_chat_completion_request(request).await
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        let url = format!("{}/models", self.base_url);

        let mut req = self.client.get(&url);
        if !self.api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.api_key));
        }
        let response = req.send().await.map_err(|e| {
            BackendError::transport(format!("Request failed: {}", e), e.is_timeout())
        })?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(BackendError::http_status_error(
                status,
                format!("API error {}: {}", status, error_text),
            ));
        }

        // Parse OpenAI-compatible models response
        #[derive(serde::Deserialize)]
        struct ModelsResponse {
            data: Vec<OpenAiModel>,
        }

        #[derive(serde::Deserialize)]
        struct OpenAiModel {
            id: String,
            #[serde(default)]
            owned_by: Option<String>,
        }

        let models_resp: ModelsResponse = response
            .json()
            .await
            .map_err(|e| BackendError::InvalidResponse(format!("Failed to parse models: {}", e)))?;

        let models = models_resp
            .data
            .into_iter()
            .map(|m| ModelInfo {
                id: m.id.clone(),
                name: Some(m.id),
                provider: m.owned_by.clone(),
                capabilities: vec!["chat".to_string()],
            })
            .collect();

        Ok(models)
    }

    fn backend_type(&self) -> BackendType {
        BackendType::Http
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kimi_compat_disables_thinking_for_known_kimi_models() {
        let mut body = serde_json::json!({
            "model": "kimi-k2.6",
            "messages": [],
            "stream": false
        });

        apply_kimi_compat("https://api.example.com/v1", "kimi-k2.6", &mut body);

        assert_eq!(body["thinking"], serde_json::json!({ "type": "disabled" }));
    }

    #[test]
    fn kimi_compat_handles_prefixed_kimi_models() {
        let mut body = serde_json::json!({
            "model": "kimi/kimi-for-coding",
            "messages": [],
            "stream": false
        });

        apply_kimi_compat(
            "https://api.example.com/v1",
            "kimi/kimi-for-coding",
            &mut body,
        );

        assert_eq!(body["thinking"], serde_json::json!({ "type": "disabled" }));
    }

    #[test]
    fn kimi_compat_disables_thinking_for_kimi_backend_aliases() {
        let mut body = serde_json::json!({
            "model": "local-dispatcher",
            "messages": [],
            "stream": false
        });

        apply_kimi_compat(
            "https://api.kimi.com/coding/v1",
            "local-dispatcher",
            &mut body,
        );

        assert_eq!(body["thinking"], serde_json::json!({ "type": "disabled" }));
    }

    #[test]
    fn kimi_compat_does_not_affect_non_kimi_models() {
        let mut body = serde_json::json!({
            "model": "codex/gpt-5.5",
            "messages": [],
            "stream": false
        });

        apply_kimi_compat("https://api.example.com/v1", "codex/gpt-5.5", &mut body);

        assert!(body.get("thinking").is_none());
    }
}
