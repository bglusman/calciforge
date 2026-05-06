//! Helicone Router - HTTP-based router for Helicone AI Gateway
//!
//! This module provides a router that sends requests to a Helicone AI Gateway
//! instance via HTTP. This is the recommended approach since ai-gateway is
//! designed as a server application, not an embedded library.

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;
use url::Url;

use crate::{
    proxy::backend::{BackendError, BackendType, ModelInfo, SecretsBackend},
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
        let request_body = ChatCompletionRequest {
            model,
            messages,
            stream: Some(stream),
            tools,
            tool_choice,
            ..Default::default()
        };

        let url = self.chat_completions_url().map_err(BackendError::from)?;

        let response = self
            .client
            .post(url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .header("Helicone-Auth", format!("Bearer {}", self.config.api_key))
            .json(&request_body)
            .send()
            .await
            .map_err(|e| BackendError::HttpError(format!("HTTP request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(BackendError::HttpError(format!(
                "Helicone API error ({}): {}",
                status, error_text
            )));
        }

        let completion_response: ChatCompletionResponse = response.json().await.map_err(|e| {
            BackendError::InvalidResponse(format!("Failed to parse response: {}", e))
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

fn helicone_chat_completions_url(base_url: &str) -> Result<Url, HeliconeError> {
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

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        self.list_models().await
    }

    fn backend_type(&self) -> BackendType {
        BackendType::Helicone
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::openai::{Choice, MessageContent, Usage};
    use mockito::Matcher;

    fn config(base_url: String) -> HeliconeRouterConfig {
        HeliconeRouterConfig {
            base_url,
            api_key: "helicone-test-key".to_string(),
            timeout_seconds: 30,
            router_name: "test".to_string(),
            enable_caching: false,
            cache_ttl_seconds: 300,
        }
    }

    #[test]
    fn test_helicone_router_creation() {
        let router = HeliconeRouter::new(config("http://localhost:8787".to_string()));
        assert!(router.is_ok());
    }

    #[test]
    fn test_default_router() {
        let router = HeliconeRouter::default();
        assert!(router.is_ok());
    }

    #[test]
    fn helicone_url_adds_v1_path_for_origin_base() {
        let url = helicone_chat_completions_url("https://ai-gateway.helicone.ai").unwrap();
        assert_eq!(
            url.as_str(),
            "https://ai-gateway.helicone.ai/v1/chat/completions"
        );
    }

    #[test]
    fn helicone_url_uses_configured_gateway_base_path() {
        let url =
            helicone_chat_completions_url("https://gateway.example.invalid/router/calciforge/")
                .unwrap();
        assert_eq!(
            url.as_str(),
            "https://gateway.example.invalid/router/calciforge/chat/completions"
        );
    }

    #[test]
    fn helicone_url_does_not_duplicate_v1_path() {
        let url = helicone_chat_completions_url("https://ai-gateway.helicone.ai/v1/").unwrap();
        assert_eq!(
            url.as_str(),
            "https://ai-gateway.helicone.ai/v1/chat/completions"
        );
    }

    #[test]
    fn helicone_url_rejects_query_or_fragment_base() {
        let err = helicone_chat_completions_url("https://ai-gateway.helicone.ai/v1?debug=true")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("query parameters or fragments"),
            "unexpected error: {err}"
        );

        let err = helicone_chat_completions_url("https://ai-gateway.helicone.ai/v1#dashboard")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("query parameters or fragments"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn chat_completion_posts_to_configured_v1_path_without_duplication() {
        let mut server = mockito::Server::new_async().await;
        let response = ChatCompletionResponse {
            id: "chatcmpl-test".to_string(),
            object: "chat.completion".to_string(),
            created: 1,
            model: "openai/gpt-4o-mini".to_string(),
            choices: vec![Choice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(MessageContent::Text("ok".to_string())),
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
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
            },
            system_fingerprint: None,
        };
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .match_header("authorization", "Bearer helicone-test-key")
            .match_header("helicone-auth", "Bearer helicone-test-key")
            .match_body(Matcher::PartialJson(serde_json::json!({
                "model": "openai/gpt-4o-mini",
                "messages": [{"role": "user", "content": "hello"}]
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(serde_json::to_string(&response).unwrap())
            .create_async()
            .await;

        let router = HeliconeRouter::new(config(format!("{}/v1/", server.url()))).unwrap();
        let result = router
            .chat_completion(
                "openai/gpt-4o-mini".to_string(),
                vec![ChatMessage {
                    role: "user".to_string(),
                    content: Some(MessageContent::Text("hello".to_string())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning: None,
                    reasoning_content: None,
                }],
                false,
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(result.model, "openai/gpt-4o-mini");
        mock.assert_async().await;
    }
}
