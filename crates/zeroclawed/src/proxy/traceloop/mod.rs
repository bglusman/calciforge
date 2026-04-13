//! Traceloop-inspired LLM routing system
//!
//! This module provides a simplified implementation of Traceloop Hub's
//! provider registry and routing system, adapted for zeroclawed.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::proxy::openai::{
    ChatCompletionResponse, ChatMessage, ToolDefinition, ToolChoice,
};
use crate::proxy::backend::BackendError;

/// Provider type enum
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ProviderType {
    OpenAI,
    Anthropic,
    DeepSeek,
    Kimi,
}

impl std::fmt::Display for ProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderType::OpenAI => write!(f, "openai"),
            ProviderType::Anthropic => write!(f, "anthropic"),
            ProviderType::DeepSeek => write!(f, "deepseek"),
            ProviderType::Kimi => write!(f, "kimi"),
        }
    }
}

impl std::str::FromStr for ProviderType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "openai" => Ok(ProviderType::OpenAI),
            "anthropic" => Ok(ProviderType::Anthropic),
            "deepseek" => Ok(ProviderType::DeepSeek),
            "kimi" | "moonshot" => Ok(ProviderType::Kimi),
            _ => Err(format!("Unknown provider type: {}", s)),
        }
    }
}

/// Provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Provider ID (e.g., "deepseek", "kimi")
    pub id: String,
    /// Provider type
    pub r#type: ProviderType,
    /// API key for the provider
    pub api_key: String,
    /// Base URL for the provider API (optional)
    pub base_url: Option<String>,
    /// Default model for this provider
    pub default_model: String,
}

/// Provider trait - similar to Traceloop's Provider trait
#[async_trait]
pub trait Provider: Send + Sync {
    fn new(config: &ProviderConfig) -> Self
    where
        Self: Sized;
    
    fn key(&self) -> String;
    
    fn r#type(&self) -> ProviderType;
    
    async fn chat_completions(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        stream: bool,
        tools: Option<Vec<ToolDefinition>>,
        tool_choice: Option<ToolChoice>,
    ) -> Result<ChatCompletionResponse, BackendError>;
}

/// Provider registry - similar to Traceloop's ProviderRegistry
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn Provider>>,
}

impl ProviderRegistry {
    pub fn new(provider_configs: &[ProviderConfig]) -> anyhow::Result<Self> {
        let mut providers = HashMap::new();

        for config in provider_configs {
            let provider: Arc<dyn Provider> = match config.r#type {
                ProviderType::OpenAI => Arc::new(OpenAIProvider::new(config)),
                ProviderType::Anthropic => Arc::new(AnthropicProvider::new(config)),
                ProviderType::DeepSeek => Arc::new(DeepSeekProvider::new(config)),
                ProviderType::Kimi => Arc::new(KimiProvider::new(config)),
            };
            providers.insert(config.id.clone(), provider);
        }

        Ok(Self { providers })
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Provider>> {
        self.providers.get(name).cloned()
    }

    pub fn providers_count(&self) -> usize {
        self.providers.len()
    }
}

/// Traceloop router - main routing interface
pub struct TraceloopRouter {
    registry: Arc<ProviderRegistry>,
    stats: RwLock<HashMap<String, RouterStats>>,
}

#[derive(Debug, Clone, Default)]
pub struct RouterStats {
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
}

impl TraceloopRouter {
    pub fn new(providers: Vec<ProviderConfig>) -> anyhow::Result<Self> {
        let registry = Arc::new(ProviderRegistry::new(&providers)?);
        
        Ok(Self {
            registry,
            stats: RwLock::new(HashMap::new()),
        })
    }

    /// Create a default router with DeepSeek and Kimi backends
    pub fn default_with_backends(
        deepseek_api_key: Option<String>,
        kimi_api_key: Option<String>,
    ) -> anyhow::Result<Self> {
        let mut providers = Vec::new();
        
        // Add DeepSeek backend if API key provided
        if let Some(api_key) = deepseek_api_key {
            providers.push(ProviderConfig {
                id: "deepseek".to_string(),
                r#type: ProviderType::DeepSeek,
                api_key,
                base_url: Some("https://api.deepseek.com/v1".to_string()),
                default_model: "deepseek-chat".to_string(),
            });
        }
        
        // Add Kimi backend using OpenAI-compatible API
        if let Some(api_key) = kimi_api_key {
            providers.push(ProviderConfig {
                id: "kimi".to_string(),
                r#type: ProviderType::Kimi,
                api_key,
                base_url: Some("https://api.moonshot.cn/v1".to_string()),
                default_model: "kimi-for-coding".to_string(),
            });
        }
        
        Self::new(providers)
    }

    /// Process a chat completion request
    pub async fn chat_completion(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        stream: bool,
        tools: Option<Vec<ToolDefinition>>,
        tool_choice: Option<ToolChoice>,
    ) -> Result<ChatCompletionResponse, BackendError> {
        // Parse model string (could be provider:model or just model name)
        let (provider_id, model_name) = if let Some((provider, model)) = model.split_once(':') {
            (provider.to_string(), model.to_string())
        } else {
            // Default to deepseek for backward compatibility
            ("deepseek".to_string(), model.to_string())
        };
        
        // Update statistics
        self.update_stats(&provider_id, true).await;
        
        // Get provider from registry
        let provider = self.registry.get(&provider_id)
            .ok_or_else(|| BackendError::NotAvailable(format!("Provider '{}' not found in registry", provider_id)))?;
        
        // Make the request
        let result = provider.chat_completions(
            model_name,
            messages,
            stream,
            tools,
            tool_choice,
        ).await;
        
        // Update success/failure stats
        match &result {
            Ok(_) => self.record_success(&provider_id).await,
            Err(_) => self.record_failure(&provider_id).await,
        }
        
        result
    }

    /// Get the number of providers in the registry
    pub fn providers_count(&self) -> usize {
        self.registry.providers_count()
    }

    /// List available models
    pub async fn list_models(&self) -> Result<Vec<crate::proxy::backend::ModelInfo>, BackendError> {
        // For now, return a simple list
        // In a full implementation, we would query each provider
        let models = vec![
            crate::proxy::backend::ModelInfo {
                id: "deepseek:deepseek-chat".to_string(),
                name: Some("DeepSeek Chat".to_string()),
                provider: Some("deepseek".to_string()),
                capabilities: vec!["chat".to_string()],
            },
            crate::proxy::backend::ModelInfo {
                id: "kimi:kimi-for-coding".to_string(),
                name: Some("Kimi for Coding".to_string()),
                provider: Some("kimi".to_string()),
                capabilities: vec!["chat".to_string()],
            },
        ];
        
        Ok(models)
    }

    async fn update_stats(&self, provider_id: &str, _in_progress: bool) {
        let mut stats = self.stats.write().await;
        let provider_stats = stats.entry(provider_id.to_string()).or_insert_with(RouterStats::default);
        provider_stats.total_requests += 1;
    }

    async fn record_success(&self, provider_id: &str) {
        let mut stats = self.stats.write().await;
        if let Some(provider_stats) = stats.get_mut(provider_id) {
            provider_stats.successful_requests += 1;
        }
    }

    async fn record_failure(&self, provider_id: &str) {
        let mut stats = self.stats.write().await;
        if let Some(provider_stats) = stats.get_mut(provider_id) {
            provider_stats.failed_requests += 1;
        }
    }
}

// Provider implementations will go in separate files
mod openai;
mod anthropic;
mod deepseek;
mod kimi;

// Re-export provider implementations
pub use openai::OpenAIProvider;
pub use anthropic::AnthropicProvider;
pub use deepseek::DeepSeekProvider;
pub use kimi::KimiProvider;