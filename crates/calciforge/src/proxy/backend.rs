//! Unified backend interface for the model gateway
//!
//! Provides a trait-based abstraction for different OneCLI integration methods:
//! - Embedded (spawns subprocess)
//! - Library (uses OneCLI as library)
//! - HTTP (HTTP to OneCLI server)
//! - Helicone (HTTP to Helicone AI Gateway)
//! - Mock (for testing)

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::sync::Arc;

use crate::proxy::openai::{ChatCompletionResponse, MessageContent};

// Helicone router (embedded library)
use super::helicone_router;

/// Errors that can occur in backend operations
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum BackendError {
    #[error("OneCLI execution failed: {0}")]
    ExecutionFailed(String),

    #[error("OneCLI not found or not executable")]
    SecretsNotFound,

    #[error("HTTP request failed: {0}")]
    HttpError(String),

    #[error("Invalid response from backend: {0}")]
    InvalidResponse(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Backend not available: {0}")]
    NotAvailable(String),
}

/// Unified backend trait for OneCLI integration
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

    /// List available models
    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError>;

    /// Get backend type for logging/debugging
    fn backend_type(&self) -> BackendType;
}

/// Backend types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendType {
    /// Spawn OneCLI as subprocess
    Embedded,
    /// Use OneCLI as library
    Library,
    /// HTTP to OneCLI server
    Http,
    /// HTTP to Helicone AI Gateway
    Helicone,
    /// Mock backend for testing
    Mock,
}

impl std::fmt::Display for BackendType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendType::Embedded => write!(f, "embedded"),
            BackendType::Library => write!(f, "library"),
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

    // Embedded backend config
    pub command: Option<String>,
    pub args: Option<Vec<String>>,

    // HTTP backend config
    pub url: Option<String>,
    pub api_key: Option<String>,
    pub timeout_seconds: Option<u64>,
    pub headers: Option<std::collections::HashMap<String, String>>,

    // Helicone backend config
    pub helicone_url: Option<String>,
    pub helicone_api_key: Option<String>,
    pub helicone_router_name: Option<String>,

    // Library backend config
    pub config_path: Option<String>,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            backend_type: BackendType::Mock,
            command: Some("secrets".to_string()),
            args: Some(vec![
                "--config".to_string(),
                "~/.config/secrets.toml".to_string(),
            ]),
            url: Some("http://localhost:8081".to_string()),
            api_key: None,
            timeout_seconds: Some(30),
            headers: None,
            helicone_url: Some("http://localhost:8080".to_string()),
            helicone_api_key: None,
            helicone_router_name: None,
            config_path: Some("~/.config/secrets.toml".to_string()),
        }
    }
}

/// Factory function to create backend based on config
pub fn create_backend(config: &BackendConfig) -> Result<Arc<dyn SecretsBackend>, BackendError> {
    match config.backend_type {
        BackendType::Embedded => {
            let command = config.command.clone().ok_or_else(|| {
                BackendError::ConfigError("Missing command for embedded backend".to_string())
            })?;
            let args = config.args.clone().unwrap_or_default();
            Ok(Arc::new(EmbeddedBackend::new(command, args)))
        }
        BackendType::Library => {
            let config_path = config.config_path.clone().ok_or_else(|| {
                BackendError::ConfigError("Missing config_path for library backend".to_string())
            })?;
            Ok(Arc::new(LibraryBackend::new(config_path)))
        }
        BackendType::Http => {
            let url = config.url.clone().ok_or_else(|| {
                BackendError::ConfigError("Missing url for HTTP backend".to_string())
            })?;
            let api_key = config.api_key.clone().unwrap_or_default();
            let timeout = config.timeout_seconds.unwrap_or(30);
            let headers = config.headers.clone();
            Ok(Arc::new(HttpBackend::new(url, api_key, timeout, headers)))
        }
        BackendType::Helicone => {
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
            };
            let router = helicone_router::HeliconeRouter::new(helicone_config).map_err(|e| {
                BackendError::ConfigError(format!("Failed to create Helicone router: {}", e))
            })?;
            Ok(Arc::new(router))
        }
        BackendType::Mock => Ok(Arc::new(MockBackend::new())),
    }
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

// Embedded backend implementation (spawns subprocess)
#[allow(dead_code)]
pub struct EmbeddedBackend {
    command: String,
    args: Vec<String>,
}

impl EmbeddedBackend {
    pub fn new(command: String, args: Vec<String>) -> Self {
        Self { command, args }
    }
}

#[async_trait::async_trait]
impl SecretsBackend for EmbeddedBackend {
    async fn chat_completion(
        &self,
        _model: String,
        _messages: Vec<ChatMessage>,
        _stream: bool,
        _tools: Option<Vec<crate::proxy::openai::ToolDefinition>>,
        _tool_choice: Option<crate::proxy::openai::ToolChoice>,
    ) -> Result<ChatCompletionResponse, BackendError> {
        // TODO: Implement actual OneCLI subprocess execution
        Err(BackendError::NotAvailable(
            "Embedded backend not yet implemented".to_string(),
        ))
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        // TODO: Implement actual OneCLI subprocess execution
        Err(BackendError::NotAvailable(
            "Embedded backend not yet implemented".to_string(),
        ))
    }

    fn backend_type(&self) -> BackendType {
        BackendType::Embedded
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

#[allow(dead_code)]
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

    /// Create backend with OpenRouter configuration
    pub fn openrouter(api_key: String) -> Self {
        Self::new(
            "https://openrouter.ai/api/v1".to_string(),
            api_key,
            120,
            None,
        )
    }

    /// Create backend with local OpenClaw gateway
    pub fn openclaw_local(api_key: String) -> Self {
        Self::new("http://127.0.0.1:18789/v1".to_string(), api_key, 300, None)
    }
}

fn is_kimi_model(model: &str) -> bool {
    let model = model.trim_start_matches("kimi/");
    model.starts_with("kimi-")
}

fn apply_kimi_compat(model: &str, request_body: &mut serde_json::Value) {
    if is_kimi_model(model) && request_body.get("thinking").is_none() {
        // Kimi K2.5/K2.6 enable thinking by default. In tool-call conversations
        // the API then requires every prior assistant tool-call message to carry
        // reasoning_content. Many OpenAI-compatible clients do not preserve that
        // provider-specific field, so disable thinking unless a future config
        // path explicitly opts back in.
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
        let url = format!("{}/chat/completions", self.base_url);

        // Force non-streaming - streaming responses require SSE parsing
        let _ = stream; // Acknowledge parameter but don't use it for now

        // Build request body with optional tools
        let mut request_body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": false,
        });
        apply_kimi_compat(&model, &mut request_body);

        // Add tools if present
        if let Some(tools) = tools {
            request_body["tools"] = serde_json::to_value(tools).unwrap_or(serde_json::Value::Null);
        }

        // Add tool_choice if present
        if let Some(tool_choice) = tool_choice {
            request_body["tool_choice"] =
                serde_json::to_value(tool_choice).unwrap_or(serde_json::Value::Null);
        }

        let mut request_builder = self
            .client
            .post(&url)
            .header("Content-Type", "application/json");

        if !self.api_key.is_empty() {
            request_builder =
                request_builder.header("Authorization", format!("Bearer {}", self.api_key));
        }

        // Add custom headers from config
        for (key, value) in &self.headers {
            request_builder = request_builder.header(key, value);
        }

        let response = request_builder
            .json(&request_body)
            .send()
            .await
            .map_err(|e| BackendError::HttpError(format!("Request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(BackendError::HttpError(format!(
                "API error {}: {}",
                status, error_text
            )));
        }

        let completion: ChatCompletionResponse = response.json().await.map_err(|e| {
            BackendError::InvalidResponse(format!("Failed to parse response: {}", e))
        })?;

        Ok(completion)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        let url = format!("{}/models", self.base_url);

        let mut req = self.client.get(&url);
        if !self.api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.api_key));
        }
        let response = req
            .send()
            .await
            .map_err(|e| BackendError::HttpError(format!("Request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(BackendError::HttpError(format!(
                "API error {}: {}",
                status, error_text
            )));
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

// Library backend implementation
#[allow(dead_code)]
pub struct LibraryBackend {
    config_path: String,
}

impl LibraryBackend {
    pub fn new(config_path: String) -> Self {
        Self { config_path }
    }
}

#[async_trait::async_trait]
impl SecretsBackend for LibraryBackend {
    async fn chat_completion(
        &self,
        _model: String,
        _messages: Vec<ChatMessage>,
        _stream: bool,
        _tools: Option<Vec<crate::proxy::openai::ToolDefinition>>,
        _tool_choice: Option<crate::proxy::openai::ToolChoice>,
    ) -> Result<ChatCompletionResponse, BackendError> {
        // TODO: Implement actual OneCLI library integration
        Err(BackendError::NotAvailable(
            "Library backend not yet implemented".to_string(),
        ))
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        // TODO: Implement actual OneCLI library integration
        Err(BackendError::NotAvailable(
            "Library backend not yet implemented".to_string(),
        ))
    }

    fn backend_type(&self) -> BackendType {
        BackendType::Library
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

        apply_kimi_compat("kimi-k2.6", &mut body);

        assert_eq!(body["thinking"], serde_json::json!({ "type": "disabled" }));
    }

    #[test]
    fn kimi_compat_handles_prefixed_kimi_models() {
        let mut body = serde_json::json!({
            "model": "kimi/kimi-for-coding",
            "messages": [],
            "stream": false
        });

        apply_kimi_compat("kimi/kimi-for-coding", &mut body);

        assert_eq!(body["thinking"], serde_json::json!({ "type": "disabled" }));
    }

    #[test]
    fn kimi_compat_does_not_affect_non_kimi_models() {
        let mut body = serde_json::json!({
            "model": "codex/gpt-5.5",
            "messages": [],
            "stream": false
        });

        apply_kimi_compat("codex/gpt-5.5", &mut body);

        assert!(body.get("thinking").is_none());
    }
}
