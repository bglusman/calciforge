//! Helicone Router - HTTP-based router for Helicone AI Gateway
//!
//! This module provides a router that sends requests to a Helicone AI Gateway
//! instance via HTTP. This is the recommended approach since ai-gateway is
//! designed as a server application, not an embedded library.

use async_trait::async_trait;
use reqwest::{
    Client,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use thiserror::Error;
use url::Url;

use crate::{
    config::GatewayRetryConfig,
    proxy::backend::{BackendError, BackendType, ModelInfo, SecretsBackend},
    proxy::helicone_streaming::parse_streaming_chat_completion,
    proxy::openai::{
        ChatCompletionRequest, ChatCompletionResponse, ChatMessage, ToolChoice, ToolDefinition,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeliconeRouterConfig {
    /// Base URL of the Helicone AI Gateway instance
    pub base_url: String,
    /// API key for Helicone
    pub api_key: String,
    /// Timeout in seconds for requests
    pub timeout_seconds: u64,
    /// Router name for identification
    pub router_name: String,
    /// Enable response caching
    pub enable_caching: bool,
    /// Cache TTL in seconds
    pub cache_ttl_seconds: u64,
    /// Custom headers forwarded to the Helicone AI Gateway.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Gateway retry policy. Mapped to Helicone retry headers when enabled.
    #[serde(default)]
    pub retry: GatewayRetryConfig,
}

impl Default for HeliconeRouterConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:8787".to_string(),
            api_key: "".to_string(),
            timeout_seconds: 30,
            router_name: "helicone".to_string(),
            enable_caching: false,
            cache_ttl_seconds: 300,
            headers: HashMap::new(),
            retry: GatewayRetryConfig::default(),
        }
    }
}

#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum HeliconeError {
    #[error("Configuration error: {0}")]
    Config(String),
    #[error("HTTP client error: {0}")]
    HttpClient(String),
    #[error("Request error: {0}")]
    Request(String),
    #[error("Response error: {0}")]
    Response(String),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Timeout error: {0}")]
    Timeout(String),
}

impl From<HeliconeError> for BackendError {
    fn from(err: HeliconeError) -> Self {
        BackendError::ConfigError(err.to_string())
    }
}

#[derive(Debug)]
pub struct HeliconeRouter {
    config: HeliconeRouterConfig,
    client: Client,
}

impl HeliconeRouter {
    pub fn new(config: HeliconeRouterConfig) -> Result<Self, HeliconeError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .build()
            .map_err(|e| {
                HeliconeError::HttpClient(format!("Failed to create HTTP client: {}", e))
            })?;

        Ok(Self { config, client })
    }

    fn chat_completions_url(&self) -> Result<Url, HeliconeError> {
        helicone_chat_completions_url(&self.config.base_url)
    }

    /// Create a default router with standard configuration
    #[allow(dead_code)]
    pub fn default() -> Result<Self, HeliconeError> {
        Self::new(HeliconeRouterConfig::default())
    }

    pub async fn chat_completion(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        stream: bool,
        tools: Option<Vec<ToolDefinition>>,
        tool_choice: Option<ToolChoice>,
    ) -> Result<ChatCompletionResponse, BackendError> {
        self.chat_completion_request(ChatCompletionRequest {
            model,
            messages,
            stream: Some(stream),
            tools,
            tool_choice,
            ..Default::default()
        })
        .await
    }

    pub async fn chat_completion_request(
        &self,
        request_body: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, BackendError> {
        let url = self.chat_completions_url().map_err(BackendError::from)?;
        let url_for_error = url.as_str().to_string();
        let model_for_error = request_body.model.clone();

        let mut headers = HeaderMap::new();
        for (name, value) in &self.config.headers {
            let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
                BackendError::ConfigError(format!("Invalid Helicone custom header '{name}': {e}"))
            })?;
            let header_value = HeaderValue::from_str(value).map_err(|e| {
                BackendError::ConfigError(format!(
                    "Invalid value for Helicone custom header '{name}': {e}"
                ))
            })?;
            headers.insert(header_name, header_value);
        }
        if self.config.retry.enabled {
            headers.insert(
                HeaderName::from_static("helicone-retry-enabled"),
                HeaderValue::from_static("true"),
            );
            headers.insert(
                HeaderName::from_static("helicone-retry-num"),
                HeaderValue::from_str(&self.config.retry.max_retries.to_string()).map_err(|e| {
                    BackendError::ConfigError(format!("Invalid Helicone retry count: {e}"))
                })?,
            );
            headers.insert(
                HeaderName::from_static("helicone-retry-min-timeout"),
                HeaderValue::from_str(&self.config.retry.min_timeout_ms.to_string()).map_err(
                    |e| {
                        BackendError::ConfigError(format!(
                            "Invalid Helicone retry minimum timeout: {e}"
                        ))
                    },
                )?,
            );
            headers.insert(
                HeaderName::from_static("helicone-retry-max-timeout"),
                HeaderValue::from_str(&self.config.retry.max_timeout_ms.to_string()).map_err(
                    |e| {
                        BackendError::ConfigError(format!(
                            "Invalid Helicone retry maximum timeout: {e}"
                        ))
                    },
                )?,
            );
            headers.insert(
                HeaderName::from_static("helicone-retry-factor"),
                HeaderValue::from_str(&self.config.retry.factor.to_string()).map_err(|e| {
                    BackendError::ConfigError(format!("Invalid Helicone retry factor: {e}"))
                })?,
            );
        }
        let bearer =
            HeaderValue::from_str(&format!("Bearer {}", self.config.api_key)).map_err(|e| {
                BackendError::ConfigError(format!("Invalid Helicone API key for auth header: {e}"))
            })?;
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(AUTHORIZATION, bearer.clone());
        headers.insert(HeaderName::from_static("helicone-auth"), bearer);

        let response = self
            .client
            .post(url)
            .headers(headers)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| {
                BackendError::transport(
                    format!(
                        "Helicone request to {} for model '{}' failed: {}",
                        url_for_error, model_for_error, e
                    ),
                    e.is_timeout(),
                )
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(BackendError::http_status_error(
                status,
                format!(
                    "Helicone gateway returned {} for model '{}': {}",
                    status,
                    model_for_error,
                    truncate_error_body(error_text.trim())
                ),
            ));
        }

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if content_type.contains("text/event-stream") {
            let body = response.text().await.map_err(|e| {
                BackendError::transport(
                    format!(
                        "Failed to read Helicone streaming response for model '{}': {}",
                        model_for_error, e
                    ),
                    e.is_timeout(),
                )
            })?;
            return parse_streaming_chat_completion(&body, &model_for_error);
        }

        let completion_response: ChatCompletionResponse = response.json().await.map_err(|e| {
            BackendError::InvalidResponse(format!(
                "Failed to parse Helicone response for model '{}': {}",
                model_for_error, e
            ))
        })?;

        Ok(completion_response)
    }

    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        // Helicone doesn't have a standard models endpoint, so we return
        // a placeholder list or fetch from the underlying provider
        // For now, return an empty list
        Ok(vec![])
    }
}

pub(super) fn helicone_chat_completions_url(base_url: &str) -> Result<Url, HeliconeError> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return Err(HeliconeError::Config(
            "Helicone base_url cannot be blank".to_string(),
        ));
    }

    let mut url = Url::parse(trimmed).map_err(|e| {
        HeliconeError::Config(format!(
            "Helicone base_url '{}' is invalid: {}",
            base_url, e
        ))
    })?;
    if url.query().is_some() || url.fragment().is_some() {
        return Err(HeliconeError::Config(
            "Helicone base_url must not include query parameters or fragments".to_string(),
        ));
    }

    let path = url.path().trim_end_matches('/');
    let chat_path = if path.is_empty() {
        "/v1/chat/completions".to_string()
    } else if path.ends_with("/chat/completions") {
        path.to_string()
    } else {
        format!("{path}/chat/completions")
    };
    url.set_path(&chat_path);
    Ok(url)
}

fn truncate_error_body(body: &str) -> String {
    const MAX_ERROR_BODY_CHARS: usize = 1024;
    let mut chars = body.chars();
    let truncated: String = chars.by_ref().take(MAX_ERROR_BODY_CHARS).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

// ---------------------------------------------------------------------------
// Router trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
#[allow(dead_code)]
pub trait Router: Send + Sync {
    async fn chat_completion(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        stream: bool,
        tools: Option<Vec<ToolDefinition>>,
        tool_choice: Option<ToolChoice>,
    ) -> Result<ChatCompletionResponse, BackendError>;

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError>;
}

#[async_trait]
impl Router for HeliconeRouter {
    async fn chat_completion(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        stream: bool,
        tools: Option<Vec<ToolDefinition>>,
        tool_choice: Option<ToolChoice>,
    ) -> Result<ChatCompletionResponse, BackendError> {
        self.chat_completion(model, messages, stream, tools, tool_choice)
            .await
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        self.list_models().await
    }
}

// ---------------------------------------------------------------------------
// SecretsBackend implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl SecretsBackend for HeliconeRouter {
    async fn chat_completion(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        stream: bool,
        tools: Option<Vec<ToolDefinition>>,
        tool_choice: Option<ToolChoice>,
    ) -> Result<ChatCompletionResponse, BackendError> {
        self.chat_completion(model, messages, stream, tools, tool_choice)
            .await
    }

    async fn chat_completion_request(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, BackendError> {
        HeliconeRouter::chat_completion_request(self, request).await
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        self.list_models().await
    }

    fn backend_type(&self) -> BackendType {
        BackendType::Helicone
    }
}
