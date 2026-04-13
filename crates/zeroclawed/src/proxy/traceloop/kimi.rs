//! Kimi (Moonshot AI) provider implementation
//!
//! Kimi uses an OpenAI-compatible API.

#![allow(dead_code)]

use super::{Provider, ProviderConfig, ProviderType};
use crate::proxy::openai::{
    ChatCompletionResponse, ChatMessage, ToolDefinition, ToolChoice,
};
use crate::proxy::backend::BackendError;
use async_trait::async_trait;
use reqwest::Client;

pub struct KimiProvider {
    config: ProviderConfig,
    http_client: Client,
}

impl KimiProvider {
    fn base_url(&self) -> String {
        self.config
            .base_url
            .clone()
            .unwrap_or_else(|| "https://api.moonshot.cn/v1".to_string())
    }
}

#[async_trait]
impl Provider for KimiProvider {
    fn new(config: &ProviderConfig) -> Self {
        Self {
            config: config.clone(),
            http_client: Client::new(),
        }
    }

    fn key(&self) -> String {
        self.config.id.clone()
    }

    fn r#type(&self) -> ProviderType {
        ProviderType::Kimi
    }

    async fn chat_completions(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        stream: bool,
        tools: Option<Vec<ToolDefinition>>,
        tool_choice: Option<ToolChoice>,
    ) -> Result<ChatCompletionResponse, BackendError> {
        // Kimi API is OpenAI-compatible, so we can reuse the OpenAI provider logic
        // For simplicity, we'll create a temporary OpenAI provider with Kimi's config
        let openai_config = ProviderConfig {
            id: self.config.id.clone(),
            r#type: ProviderType::OpenAI,
            api_key: self.config.api_key.clone(),
            base_url: Some(self.base_url()),
            default_model: self.config.default_model.clone(),
        };
        
        let openai_provider = super::openai::OpenAIProvider::new(&openai_config);
        
        openai_provider.chat_completions(model, messages, stream, tools, tool_choice).await
    }
}