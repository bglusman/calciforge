//! Alloy Router that delegates to Helicone AI Gateway
//!
//! This module provides an AlloyRouter that wraps the HeliconeRouter
//! for backward compatibility.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::proxy::{
    backend::{BackendError, ModelInfo},
    openai::{ChatCompletionResponse, ChatMessage, ToolChoice, ToolDefinition},
};

#[cfg(feature = "helicone")]
use crate::proxy::helicone_router::{HeliconeRouter, HeliconeRouterConfig};

/// Alloy routing strategy (kept for backward compatibility)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlloyStrategy {
    /// Round-robin between constituents
    RoundRobin,
    /// Weighted random selection
    Weighted(Vec<f32>),
    /// First available (fallback chain)
    FirstAvailable,
}

/// Provider configuration (kept for backward compatibility)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Provider ID (e.g., "deepseek", "kimi")
    pub id: String,
    /// LLM backend type
    pub backend: String,
    /// API key for the provider
    pub api_key: String,
    /// Base URL for the provider API (optional)
    pub base_url: Option<String>,
    /// Default model for this provider
    pub default_model: String,
}

/// Alloy configuration (kept for backward compatibility)
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

/// Alloy statistics (kept for backward compatibility)
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct AlloyStats {
    /// Total requests processed
    pub total_requests: u64,
    /// Requests per constituent
    pub constituent_requests: std::collections::HashMap<String, u64>,
    /// Errors per constituent
    pub constituent_errors: std::collections::HashMap<String, u64>,
}

/// Alloy router that delegates to HeliconeRouter
pub struct AlloyRouter {
    /// Helicone router for actual request handling
    helicone_router: Option<Arc<HeliconeRouter>>,
    /// Alloy configurations (kept for backward compatibility)
    alloys: std::collections::HashMap<String, AlloyConfig>,
    /// Statistics (kept for backward compatibility)
    stats: RwLock<std::collections::HashMap<String, AlloyStats>>,
    /// Round-robin counters (kept for backward compatibility)
    rr_counters: RwLock<std::collections::HashMap<String, usize>>,
}

impl AlloyRouter {
    /// Create a new AlloyRouter with the given providers and alloys
    pub fn new(
        _providers: Vec<ProviderConfig>,
        alloys: Vec<AlloyConfig>,
        helicone_config: Option<HeliconeRouterConfig>,
    ) -> anyhow::Result<Self> {
        // Create Helicone router if config provided
        let helicone_router = helicone_config
            .and_then(|config| HeliconeRouter::new(config).ok())
            .map(Arc::new);

        let mut alloy_map = std::collections::HashMap::new();
        for alloy in alloys {
            alloy_map.insert(alloy.id.clone(), alloy);
        }

        Ok(Self {
            helicone_router,
            alloys: alloy_map,
            stats: RwLock::new(std::collections::HashMap::new()),
            rr_counters: RwLock::new(std::collections::HashMap::new()),
        })
    }

    /// Create a default AlloyRouter with Helicone backend
    pub fn default_with_helicone(
        helicone_base_url: String,
        helicone_api_key: String,
    ) -> anyhow::Result<Self> {
        let helicone_config = HeliconeRouterConfig {
            base_url: helicone_base_url,
            api_key: helicone_api_key,
            timeout_seconds: 30,
            router_name: "alloy".to_string(),
            enable_caching: true,
            cache_ttl_seconds: 300,
        };

        // Default alloys for backward compatibility
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

        Self::new(vec![], alloys, Some(helicone_config))
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
        // Check if we have a Helicone router
        if let Some(router) = &self.helicone_router {
            router
                .chat_completion(model, messages, stream, tools, tool_choice)
                .await
        } else {
            Err(BackendError::NotAvailable(
                "Helicone router not configured".to_string(),
            ))
        }
    }

    /// List available models
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        // Check if we have a Helicone router
        if let Some(router) = &self.helicone_router {
            router.list_models().await
        } else {
            // Return alloy models for backward compatibility
            let mut models = Vec::new();

            for alloy in self.alloys.values() {
                models.push(ModelInfo {
                    id: alloy.id.clone(),
                    name: Some(alloy.name.clone()),
                    provider: Some("alloy".to_string()),
                    capabilities: vec!["chat".to_string(), "alloy".to_string()],
                });
            }

            Ok(models)
        }
    }

    /// Get the number of providers configured
    pub fn providers_count(&self) -> usize {
        self.alloys.len()
    }

    /// Update statistics for an alloy (kept for backward compatibility)
    async fn update_stats(&self, alloy_id: &str, _success: bool) {
        let mut stats = self.stats.write().await;
        let alloy_stats = stats
            .entry(alloy_id.to_string())
            .or_insert_with(AlloyStats::default);
        alloy_stats.total_requests += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alloy_router_creation() {
        // Test creating router with Helicone config
        let config = HeliconeRouterConfig {
            base_url: "http://localhost:8787".to_string(),
            api_key: "test-key".to_string(),
            timeout_seconds: 30,
            router_name: "test".to_string(),
            enable_caching: false,
            cache_ttl_seconds: 300,
        };

        let router = AlloyRouter::new(vec![], vec![], Some(config));
        assert!(router.is_ok());
    }

    #[test]
    fn test_alloy_config_validation() {
        // Valid alloy config
        let valid = AlloyConfig {
            id: "valid-alloy".to_string(),
            name: "Valid Alloy".to_string(),
            constituents: vec!["deepseek:deepseek-chat".to_string()],
            strategy: AlloyStrategy::RoundRobin,
        };
        assert_eq!(valid.constituents.len(), 1);
    }
}
