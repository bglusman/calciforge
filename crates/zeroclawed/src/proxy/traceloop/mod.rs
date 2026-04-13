//! Traceloop-inspired LLM routing system
//!
//! This module provides a simplified implementation of Traceloop Hub's
//! provider registry and routing system, adapted for zeroclawed.

#![allow(dead_code)]

use std::collections::HashMap;
use crate::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use sha2::{Sha256, Digest};

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

/// Cache entry with TTL
#[derive(Debug, Clone)]
struct CacheEntry {
    response: ChatCompletionResponse,
    expires_at: Instant,
}

/// Traceloop router - main routing interface with caching
pub struct TraceloopRouter {
    registry: Arc<ProviderRegistry>,
    stats: RwLock<HashMap<String, RouterStats>>,
    latency_stats: RwLock<HashMap<String, LatencyStats>>,
    cache: RwLock<HashMap<String, CacheEntry>>,
}

/// Latency statistics for smart routing
#[derive(Debug, Clone)]
pub struct LatencyStats {
    total_requests: u64,
    total_latency_ms: u64,
    last_latency_ms: Option<u64>,
    last_updated: Instant,
}

impl Default for LatencyStats {
    fn default() -> Self {
        Self {
            total_requests: 0,
            total_latency_ms: 0,
            last_latency_ms: None,
            last_updated: Instant::now(),
        }
    }
}

impl LatencyStats {
    fn record_latency(&mut self, latency_ms: u64) {
        self.total_requests += 1;
        self.total_latency_ms += latency_ms;
        self.last_latency_ms = Some(latency_ms);
        self.last_updated = Instant::now();
    }
    
    fn average_latency_ms(&self) -> Option<u64> {
        if self.total_requests > 0 {
            Some(self.total_latency_ms / self.total_requests)
        } else {
            None
        }
    }
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
            latency_stats: RwLock::new(HashMap::new()),
            cache: RwLock::new(HashMap::new()),
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

    /// Generate cache key from request parameters
    fn generate_cache_key(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tools: &Option<Vec<ToolDefinition>>,
        tool_choice: &Option<ToolChoice>,
    ) -> String {
        let mut hasher = Sha256::new();
        
        // Hash model
        hasher.update(model.as_bytes());
        
        // Hash messages
        for msg in messages {
            hasher.update(msg.role.as_bytes());
            if let Some(content) = &msg.content {
                if let Some(text) = content.to_text() {
                    hasher.update(text.as_bytes());
                }
            }
        }
        
        // Hash tools if present
        if let Some(tools) = tools {
            for tool in tools {
                hasher.update(tool.function.name.as_bytes());
                if let Some(desc) = &tool.function.description {
                    hasher.update(desc.as_bytes());
                }
                hasher.update(tool.function.parameters.to_string().as_bytes());
            }
        }
        
        // Hash tool choice if present
        if let Some(tool_choice) = tool_choice {
            match tool_choice {
                ToolChoice::Mode(mode) => hasher.update(mode.as_bytes()),
                ToolChoice::Specific { r#type, function } => {
                    hasher.update(r#type.as_bytes());
                    hasher.update(function.name.as_bytes());
                }
            }
        }
        
        format!("{:x}", hasher.finalize())
    }
    
    /// Get cached response if available and not expired
    async fn get_cached_response(
        &self,
        cache_key: &str,
    ) -> Option<ChatCompletionResponse> {
        let cache = self.cache.read().await;
        if let Some(entry) = cache.get(cache_key) {
            if entry.expires_at > Instant::now() {
                // Cache hit!
                return Some(entry.response.clone());
            }
        }
        None
    }
    
    /// Store response in cache
    async fn cache_response(
        &self,
        cache_key: String,
        response: ChatCompletionResponse,
        ttl_seconds: u64,
    ) {
        let mut cache = self.cache.write().await;
        cache.insert(cache_key, CacheEntry {
            response,
            expires_at: Instant::now() + Duration::from_secs(ttl_seconds),
        });
    }
    
    /// Clean expired cache entries
    async fn clean_expired_cache(&self) {
        let mut cache = self.cache.write().await;
        let now = Instant::now();
        cache.retain(|_, entry| entry.expires_at > now);
    }
    
    /// Get cache statistics
    pub async fn cache_statistics(&self) -> (usize, usize, usize) {
        let cache = self.cache.read().await;
        let total_entries = cache.len();
        
        // Count expired entries
        let now = Instant::now();
        let expired_entries = cache.values()
            .filter(|entry| entry.expires_at <= now)
            .count();
        
        // Count valid entries
        let valid_entries = total_entries - expired_entries;
        
        (total_entries, valid_entries, expired_entries)
    }
    
    /// Get latency statistics for all providers
    pub async fn latency_statistics(&self) -> HashMap<String, LatencyStats> {
        self.latency_stats.read().await.clone()
    }
    
    /// Get router statistics
    pub async fn router_statistics(&self) -> HashMap<String, RouterStats> {
        self.stats.read().await.clone()
    }
    
    /// Select best provider using P2C (Power of Two Choices) algorithm
    async fn select_best_provider(&self, model: &str) -> Option<String> {
        // Parse model to get provider candidates
        let (requested_provider, _) = if let Some((provider, _)) = model.split_once(':') {
            (provider.to_string(), "")
        } else {
            // Model doesn't specify provider, try all available
            return self.select_fastest_provider().await;
        };
        
        // If specific provider requested, use it
        Some(requested_provider)
    }
    
    /// Select fastest provider based on latency stats
    async fn select_fastest_provider(&self) -> Option<String> {
        let latency_stats = self.latency_stats.read().await;
        
        // Get providers with latency data
        let mut providers_with_latency: Vec<(String, u64)> = latency_stats
            .iter()
            .filter_map(|(provider_id, stats)| {
                stats.average_latency_ms().map(|latency| (provider_id.clone(), latency))
            })
            .collect();
        
        if providers_with_latency.is_empty() {
            // No latency data yet, use fallback chain
            return Some("deepseek".to_string());
        }
        
        // Sort by latency (lowest first)
        providers_with_latency.sort_by_key(|(_, latency)| *latency);
        
        // Return fastest provider
        Some(providers_with_latency[0].0.clone())
    }
    
    /// Record latency for a provider
    async fn record_latency(&self, provider_id: &str, latency_ms: u64) {
        let mut latency_stats = self.latency_stats.write().await;
        let stats = latency_stats.entry(provider_id.to_string()).or_insert_with(LatencyStats::default);
        stats.record_latency(latency_ms);
    }
    
    /// Process a chat completion request with caching and smart routing
    pub async fn chat_completion(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        stream: bool,
        tools: Option<Vec<ToolDefinition>>,
        tool_choice: Option<ToolChoice>,
    ) -> Result<ChatCompletionResponse, BackendError> {
        // Skip cache for streaming requests
        if stream {
            return self.chat_completion_without_cache(model, messages, stream, tools, tool_choice).await;
        }
        
        // Generate cache key
        let cache_key = self.generate_cache_key(&model, &messages, &tools, &tool_choice);
        
        // Check cache first
        if let Some(cached_response) = self.get_cached_response(&cache_key).await {
            // Cache hit! Return cached response
            return Ok(cached_response);
        }
        
        // Cache miss, make the request
        let start_time = Instant::now();
        let result = self.chat_completion_without_cache(model, messages, stream, tools, tool_choice).await;
        
        // Record latency if successful
        if let Ok(ref response) = result {
            let latency_ms = start_time.elapsed().as_millis() as u64;
            
            // Parse provider from model string
            let provider_id = if let Some((provider, _)) = response.model.split_once(':') {
                provider.to_string()
            } else {
                "deepseek".to_string()
            };
            
            self.record_latency(&provider_id, latency_ms).await;
            
            // Cache successful response with 5-minute TTL
            self.cache_response(cache_key, response.clone(), 300).await;
        }
        
        // Clean expired cache entries occasionally (10% chance)
        if rand::random::<f32>() < 0.1 {
            self.clean_expired_cache().await;
        }
        
        result
    }
    
    /// Process a chat completion request without caching (for streaming or internal use)
    async fn chat_completion_without_cache(
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
            // Use smart routing to select best provider
            let best_provider = self.select_best_provider(&model).await
                .ok_or_else(|| BackendError::NotAvailable("No provider available".to_string()))?;
            (best_provider, model.to_string())
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

#[cfg(test)]
mod test;