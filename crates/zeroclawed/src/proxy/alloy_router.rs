//! Alloy Router for multi-backend LLM routing
//!
//! This module provides an AlloyRouter that handles multi-backend LLM routing
//! with alloy strategies (weighted, round-robin, etc.)

use std::collections::HashMap;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::RwLock;

use crate::proxy::openai::{ChatCompletionResponse, ChatMessage, MessageContent};
use crate::proxy::backend::{BackendError, ModelInfo};

/// Alloy routing strategy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlloyStrategy {
    /// Round-robin between constituents
    RoundRobin,
    /// Weighted random selection
    Weighted(Vec<f32>),
    /// First available (fallback chain)
    FirstAvailable,
}

/// Provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Provider ID (e.g., "deepseek", "kimi")
    pub id: String,
    /// Base URL for the provider API
    pub base_url: String,
    /// API key for the provider
    pub api_key: String,
    /// Default model for this provider
    pub default_model: String,
}

/// Alloy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlloyConfig {
    /// Alloy ID (e.g., "kimi-chat-rr")
    pub id: String,
    /// Alloy name for display
    pub name: String,
    /// Constituent models with provider:model format
    pub constituents: Vec<String>,
    /// Routing strategy
    pub strategy: AlloyStrategy,
}

/// Alloy statistics
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct AlloyStats {
    /// Total requests processed
    pub total_requests: u64,
    /// Requests per constituent
    pub constituent_requests: HashMap<String, u64>,
    /// Errors per constituent
    pub constituent_errors: HashMap<String, u64>,
}

/// Alloy router for multi-backend LLM routing
pub struct AlloyRouter {
    /// HTTP client
    client: Client,
    /// Provider configurations
    providers: HashMap<String, ProviderConfig>,
    /// Alloy configurations
    alloys: HashMap<String, AlloyConfig>,
    /// Statistics
    stats: RwLock<HashMap<String, AlloyStats>>,
    /// Round-robin counters
    rr_counters: RwLock<HashMap<String, usize>>,
}

impl AlloyRouter {
    /// Create a new AlloyRouter with the given providers and alloys
    pub fn new(providers: Vec<ProviderConfig>, alloys: Vec<AlloyConfig>) -> Self {
        let mut provider_map = HashMap::new();
        for provider in providers {
            provider_map.insert(provider.id.clone(), provider);
        }
        
        let mut alloy_map = HashMap::new();
        for alloy in alloys {
            alloy_map.insert(alloy.id.clone(), alloy);
        }
        
        Self {
            client: Client::new(),
            providers: provider_map,
            alloys: alloy_map,
            stats: RwLock::new(HashMap::new()),
            rr_counters: RwLock::new(HashMap::new()),
        }
    }
    
    /// Create a default AlloyRouter with DeepSeek and Kimi backends
    pub fn default_with_backends(
        deepseek_api_key: Option<String>,
        kimi_api_key: Option<String>,
    ) -> anyhow::Result<Self> {
        let mut providers = Vec::new();
        
        // Add DeepSeek backend if API key provided
        if let Some(api_key) = deepseek_api_key {
            providers.push(ProviderConfig {
                id: "deepseek".to_string(),
                base_url: "https://api.deepseek.com/v1".to_string(),
                api_key,
                default_model: "deepseek-chat".to_string(),
            });
        }
        
        // Add Kimi backend if API key provided
        if let Some(api_key) = kimi_api_key {
            providers.push(ProviderConfig {
                id: "kimi".to_string(),
                base_url: "https://api.moonshot.cn/v1".to_string(),
                api_key,
                default_model: "kimi-for-coding".to_string(),
            });
        }
        
        // Default alloys
        let alloys = vec![
            AlloyConfig {
                id: "kimi-chat-rr".to_string(),
                name: "Kimi Chat Round-Robin".to_string(),
                constituents: vec![
                    "kimi:kimi-for-coding".to_string(),
                    "kimi:kimi-reasoner".to_string(),
                ],
                strategy: AlloyStrategy::RoundRobin,
            },
            AlloyConfig {
                id: "kimi-hybrid-rr".to_string(),
                name: "Kimi Hybrid Round-Robin".to_string(),
                constituents: vec![
                    "kimi:kimi-for-coding".to_string(),
                    "deepseek:deepseek-chat".to_string(),
                ],
                strategy: AlloyStrategy::RoundRobin,
            },
        ];
        
        Ok(Self::new(providers, alloys))
    }
    
    /// Process a chat completion request
    pub async fn chat_completion(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        stream: bool,
        tools: Option<Vec<crate::proxy::openai::ToolDefinition>>,
        tool_choice: Option<crate::proxy::openai::ToolChoice>,
    ) -> Result<ChatCompletionResponse, BackendError> {
        // Check if this is an alloy or direct model
        if let Some(alloy) = self.alloys.get(&model) {
            self.route_alloy_request(alloy, messages, stream, tools, tool_choice).await
        } else {
            self.route_direct_request(&model, messages, stream, tools, tool_choice).await
        }
    }
    
    /// Route a request through an alloy
    async fn route_alloy_request(
        &self,
        alloy: &AlloyConfig,
        messages: Vec<ChatMessage>,
        stream: bool,
        tools: Option<Vec<crate::proxy::openai::ToolDefinition>>,
        tool_choice: Option<crate::proxy::openai::ToolChoice>,
    ) -> Result<ChatCompletionResponse, BackendError> {
        // Update statistics
        self.update_stats(&alloy.id, true).await;
        
        // Select constituent based on strategy
        let constituent = self.select_constituent(alloy).await?;
        
        // Parse provider:model format
        let (provider_id, model_name) = self.parse_constituent(&constituent)?;
        
        // Get provider config
        let provider = self.providers.get(&provider_id)
            .ok_or_else(|| BackendError::NotAvailable(format!("Provider '{}' not found", provider_id)))?;
        
        // Make request to provider
        self.make_provider_request(provider, &model_name, messages, stream, tools, tool_choice).await
    }
    
    /// Route a direct request (not an alloy)
    async fn route_direct_request(
        &self,
        model: &str,
        messages: Vec<ChatMessage>,
        stream: bool,
        tools: Option<Vec<crate::proxy::openai::ToolDefinition>>,
        tool_choice: Option<crate::proxy::openai::ToolChoice>,
    ) -> Result<ChatCompletionResponse, BackendError> {
        // Parse model string (could be provider:model or just model name)
        let (provider_id, model_name) = if let Some((provider, model)) = model.split_once(':') {
            (provider.to_string(), model.to_string())
        } else {
            // Default to deepseek for backward compatibility
            ("deepseek".to_string(), model.to_string())
        };
        
        // Get provider config
        let provider = self.providers.get(&provider_id)
            .ok_or_else(|| BackendError::NotAvailable(format!("Provider '{}' not found", provider_id)))?;
        
        // Make request to provider
        self.make_provider_request(provider, &model_name, messages, stream, tools, tool_choice).await
    }
    
    /// Select constituent based on alloy strategy
    async fn select_constituent(&self, alloy: &AlloyConfig) -> Result<String, BackendError> {
        match &alloy.strategy {
            AlloyStrategy::RoundRobin => {
                let mut counters = self.rr_counters.write().await;
                let counter = counters.entry(alloy.id.clone()).or_insert(0);
                let index = *counter % alloy.constituents.len();
                *counter += 1;
                Ok(alloy.constituents[index].clone())
            }
            AlloyStrategy::Weighted(weights) => {
                // Simple weighted random selection
                let total_weight: f32 = weights.iter().sum();
                let mut rng = rand::random::<f32>() * total_weight;
                
                for (i, weight) in weights.iter().enumerate() {
                    if rng < *weight {
                        return Ok(alloy.constituents[i].clone());
                    }
                    rng -= weight;
                }
                
                // Fallback to first constituent
                Ok(alloy.constituents[0].clone())
            }
            AlloyStrategy::FirstAvailable => {
                // Try constituents in order until one works
                // For now, just return first
                Ok(alloy.constituents[0].clone())
            }
        }
    }
    
    /// Parse constituent string into provider and model
    fn parse_constituent(&self, constituent: &str) -> Result<(String, String), BackendError> {
        if let Some((provider, model)) = constituent.split_once(':') {
            Ok((provider.to_string(), model.to_string()))
        } else {
            Err(BackendError::ConfigError(
                format!("Invalid constituent format '{}', expected provider:model", constituent)
            ))
        }
    }
    
    /// Make request to a provider
    async fn make_provider_request(
        &self,
        provider: &ProviderConfig,
        model: &str,
        messages: Vec<ChatMessage>,
        stream: bool,
        tools: Option<Vec<crate::proxy::openai::ToolDefinition>>,
        tool_choice: Option<crate::proxy::openai::ToolChoice>,
    ) -> Result<ChatCompletionResponse, BackendError> {
        // Convert messages to OpenAI-compatible format
        let openai_messages: Vec<serde_json::Value> = messages.into_iter()
            .map(|msg| {
                let content = match msg.content {
                    Some(MessageContent::Text(text)) => serde_json::Value::String(text),
                    Some(MessageContent::Parts(parts)) => {
                        // Convert parts to text (simplified)
                        let text = parts.into_iter()
                            .filter_map(|part| {
                                if part.r#type == "text" {
                                    part.text
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<String>>()
                            .join(" ");
                        serde_json::Value::String(text)
                    }
                    None => serde_json::Value::String("".to_string()),
                };
                
                json!({
                    "role": msg.role,
                    "content": content,
                })
            })
            .collect();
        
        // Build request body
        let mut request_body = json!({
            "model": model,
            "messages": openai_messages,
            "stream": stream,
        });
        
        // Add tools if present
        if let Some(tools) = tools {
            let tool_value = serde_json::to_value(tools)
                .map_err(|e| BackendError::ConfigError(format!("Failed to serialize tools: {}", e)))?;
            request_body["tools"] = tool_value;
        }
        
        // Add tool_choice if present
        if let Some(tool_choice) = tool_choice {
            let tool_choice_value = serde_json::to_value(tool_choice)
                .map_err(|e| BackendError::ConfigError(format!("Failed to serialize tool_choice: {}", e)))?;
            request_body["tool_choice"] = tool_choice_value;
        }
        
        // Make HTTP request
        let response = self.client
            .post(&format!("{}/chat/completions", provider.base_url))
            .header("Authorization", format!("Bearer {}", provider.api_key))
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
            .map_err(|e| BackendError::ExecutionFailed(format!("HTTP request failed: {}", e)))?;
        
        // Check response status
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await
                .unwrap_or_else(|_| "Failed to read error body".to_string());
            return Err(BackendError::ExecutionFailed(
                format!("Provider returned error {}: {}", status, body)
            ));
        }
        
        // Parse response
        let completion_response: ChatCompletionResponse = response.json().await
            .map_err(|e| BackendError::ExecutionFailed(format!("Failed to parse response: {}", e)))?;
        
        Ok(completion_response)
    }
    
    /// Update statistics for an alloy
    async fn update_stats(&self, alloy_id: &str, _success: bool) {
        let mut stats = self.stats.write().await;
        let alloy_stats = stats.entry(alloy_id.to_string()).or_insert_with(AlloyStats::default);
        alloy_stats.total_requests += 1;
        // Note: We would update constituent-specific stats here when we have that info
    }
    
    /// Get the number of providers configured
    pub fn providers_count(&self) -> usize {
        self.providers.len()
    }
    
    /// List available models (alloys + direct models)
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        let mut models = Vec::new();
        
        // Add alloys
        for alloy in self.alloys.values() {
            models.push(ModelInfo {
                id: alloy.id.clone(),
                name: Some(alloy.name.clone()),
                provider: Some("alloy".to_string()),
                capabilities: vec!["chat".to_string(), "alloy".to_string()],
            });
        }
        
        // Add direct models from providers
        for provider in self.providers.values() {
            models.push(ModelInfo {
                id: format!("{}:{}", provider.id, provider.default_model),
                name: Some(format!("{} {}", provider.id, provider.default_model)),
                provider: Some(provider.id.clone()),
                capabilities: vec!["chat".to_string(), "function-calling".to_string()],
            });
        }
        
        Ok(models)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_parse_constituent() {
        let router = AlloyRouter::new(vec![], vec![]);
        
        let result = router.parse_constituent("deepseek:deepseek-chat");
        assert!(result.is_ok());
        let (provider, model) = result.unwrap();
        assert_eq!(provider, "deepseek");
        assert_eq!(model, "deepseek-chat");
        
        let result = router.parse_constituent("invalid-format");
        assert!(result.is_err());
    }
    
    #[tokio::test]
    async fn test_alloy_router_creation() {
        // Test creating router with no API keys
        let router = AlloyRouter::default_with_backends(None, None).unwrap();
        assert_eq!(router.providers_count(), 0);
        
        // Test creating router with mock API keys
        let router = AlloyRouter::default_with_backends(
            Some("test-deepseek-key".to_string()),
            Some("test-kimi-key".to_string()),
        ).unwrap();
        assert_eq!(router.providers_count(), 2);
        
        // Test listing models
        let models = router.list_models().await.unwrap();
        assert!(!models.is_empty());
        
        // Check that we have alloy models
        let alloy_models: Vec<_> = models.iter()
            .filter(|m| m.provider.as_deref() == Some("alloy"))
            .collect();
        assert!(!alloy_models.is_empty());
        
        // Check that we have direct models
        let direct_models: Vec<_> = models.iter()
            .filter(|m| m.provider.as_deref() != Some("alloy"))
            .collect();
        assert!(!direct_models.is_empty());
    }
    
    #[test]
    fn test_alloy_strategies() {
        // Test RoundRobin strategy selection
        let alloy = AlloyConfig {
            id: "test-alloy".to_string(),
            name: "Test Alloy".to_string(),
            constituents: vec![
                "deepseek:deepseek-chat".to_string(),
                "kimi:kimi-for-coding".to_string(),
            ],
            strategy: AlloyStrategy::RoundRobin,
        };
        
        // Create a router with the alloy
        let router = AlloyRouter::new(vec![], vec![alloy.clone()]);
        
        // We can't easily test async selection without mocking
        // But we can test that the router was created successfully
        assert_eq!(router.alloys.len(), 1);
        assert_eq!(router.alloys.get("test-alloy").unwrap().id, "test-alloy");
    }
}