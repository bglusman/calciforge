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

use crate::{
    proxy::backend::{BackendError, BackendType, ModelInfo, OneCliBackend},
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

    /// Create a default router with standard configuration
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

        let url = format!("{}/v1/chat/completions", self.config.base_url);

        let response = self
            .client
            .post(&url)
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

// ---------------------------------------------------------------------------
// Router trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
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
// OneCliBackend implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl OneCliBackend for HeliconeRouter {
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

    #[test]
    fn test_helicone_router_creation() {
        let config = HeliconeRouterConfig {
            base_url: "http://localhost:8787".to_string(),
            api_key: "test-key".to_string(),
            timeout_seconds: 30,
            router_name: "test".to_string(),
            enable_caching: false,
            cache_ttl_seconds: 300,
        };

        let router = HeliconeRouter::new(config);
        assert!(router.is_ok());
    }

    #[test]
    fn test_default_router() {
        let router = HeliconeRouter::default();
        assert!(router.is_ok());
    }
}
