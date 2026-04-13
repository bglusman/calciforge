//! GatewayBackend trait for abstracting different LLM gateway implementations.
//!
//! This module provides a unified interface for different gateway backends:
//! - Helicone (HTTP-based AI Gateway)
//! - Traceloop (observability and routing)
//! - Mock (for testing)
//! - Direct (direct provider calls)
//!
//! Each backend can be enabled via feature flags and selected via configuration.

use async_trait::async_trait;
use std::fmt::Debug;
use std::sync::Arc;

use crate::proxy::backend::{BackendError, ModelInfo, OneCliBackend};
use crate::proxy::openai::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, ToolDefinition, ToolChoice, Usage,
};

/// Configuration for a gateway backend
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// Type of gateway backend
    pub backend_type: GatewayType,
    /// Base URL for the gateway (if applicable)
    pub base_url: Option<String>,
    /// API key for the gateway (if applicable)
    pub api_key: Option<String>,
    /// Timeout in seconds
    pub timeout_seconds: u64,
    /// Additional configuration as JSON
    pub extra_config: Option<serde_json::Value>,
}

/// Type of gateway backend
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayType {
    /// Helicone AI Gateway (HTTP-based)
    Helicone,
    /// Traceloop observability gateway
    Traceloop,
    /// Direct provider calls (no gateway)
    Direct,
    /// Mock gateway for testing
    Mock,
}

impl std::str::FromStr for GatewayType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "helicone" => Ok(GatewayType::Helicone),
            "traceloop" => Ok(GatewayType::Traceloop),
            "direct" => Ok(GatewayType::Direct),
            "mock" => Ok(GatewayType::Mock),
            _ => Err(format!("Unknown gateway type: {}", s)),
        }
    }
}

impl std::fmt::Display for GatewayType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GatewayType::Helicone => write!(f, "helicone"),
            GatewayType::Traceloop => write!(f, "traceloop"),
            GatewayType::Direct => write!(f, "direct"),
            GatewayType::Mock => write!(f, "mock"),
        }
    }
}

/// Main trait for gateway backends
#[async_trait]
pub trait GatewayBackend: Send + Sync + Debug {
    /// Get the type of this gateway
    fn gateway_type(&self) -> GatewayType;

    /// Make a chat completion request
    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, BackendError>;

    /// List available models
    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError>;

    /// Get gateway configuration
    fn config(&self) -> &GatewayConfig;
}

/// Create a gateway backend from configuration
pub fn create_gateway(
    config: GatewayConfig,
    backend: Option<Arc<dyn OneCliBackend>>,
) -> Result<Box<dyn GatewayBackend>, BackendError> {
    match config.backend_type {
        #[cfg(feature = "helicone")]
        GatewayType::Helicone => {
            use crate::proxy::helicone_router::{HeliconeRouter, HeliconeRouterConfig};
            
            let helicone_config = HeliconeRouterConfig {
                base_url: config.base_url.clone().unwrap_or_else(|| "http://localhost:8787".to_string()),
                api_key: config.api_key.clone().unwrap_or_default(),
                timeout_seconds: config.timeout_seconds,
                router_name: "helicone".to_string(),
                enable_caching: false,
                cache_ttl_seconds: 300,
            };
            
            let router = HeliconeRouter::new(helicone_config)
                .map_err(|e| BackendError::ConfigError(format!("Failed to create Helicone router: {}", e)))?;
            
            Ok(Box::new(HeliconeGateway {
                config,
                router,
            }))
        }
        
        #[cfg(feature = "traceloop")]
        GatewayType::Traceloop => {
            use crate::proxy::traceloop::TraceloopRouter;
            
            // TODO: Need to pass provider configurations to TraceloopRouter
            // For now, return an error or create a minimal router
            Err(BackendError::ConfigError("Traceloop gateway not fully implemented yet".to_string()))
            
            // let router = TraceloopRouter::new(vec![])
            //     .map_err(|e| BackendError::ConfigError(format!("Failed to create Traceloop router: {}", e)))?;
            // 
            // Ok(Box::new(TraceloopGateway {
            //     config,
            //     router,
            // }))
        }
        
        #[cfg(feature = "test")]
        GatewayType::Mock => {
            Ok(Box::new(MockGateway::new(config)))
        }
        
        #[cfg(not(feature = "test"))]
        GatewayType::Mock => {
            Err(BackendError::ConfigError("Mock gateway only available in test mode".to_string()))
        }
        
        GatewayType::Direct => {
            // Direct provider calls (no gateway)
            // This requires a backend to be passed in
            let backend = backend.ok_or_else(|| 
                BackendError::ConfigError("Direct gateway requires a backend parameter".to_string())
            )?;
            
            Ok(Box::new(DirectGateway::new(config, backend)))
        }
        
        #[cfg(not(feature = "helicone"))]
        GatewayType::Helicone => {
            Err(BackendError::ConfigError("Helicone feature not enabled".to_string()))
        }
        
        #[cfg(not(feature = "traceloop"))]
        GatewayType::Traceloop => {
            Err(BackendError::ConfigError("Traceloop feature not enabled".to_string()))
        }
    }
}

// ---------------------------------------------------------------------------
// Helicone Gateway Implementation
// ---------------------------------------------------------------------------

#[cfg(feature = "helicone")]
#[derive(Debug)]
pub struct HeliconeGateway {
    config: GatewayConfig,
    router: crate::proxy::helicone_router::HeliconeRouter,
}

#[cfg(feature = "helicone")]
#[async_trait]
impl GatewayBackend for HeliconeGateway {
    fn gateway_type(&self) -> GatewayType {
        GatewayType::Helicone
    }

    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, BackendError> {
        // Extract parameters from request to pass to HeliconeRouter
        // Note: HeliconeRouter uses the old parameter-based API
        self.router.chat_completion(
            request.model,
            request.messages,
            request.stream.unwrap_or(false),
            request.tools,
            request.tool_choice,
        ).await
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        self.router.list_models().await
    }

    fn config(&self) -> &GatewayConfig {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// Direct Gateway Implementation (wraps existing OneCliBackend)
// ---------------------------------------------------------------------------

/// Direct gateway that wraps an existing OneCliBackend
pub struct DirectGateway {
    config: GatewayConfig,
    backend: Arc<dyn OneCliBackend>,
}

impl Debug for DirectGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectGateway")
            .field("config", &self.config)
            .field("backend_type", &self.backend.backend_type())
            .finish()
    }
}

impl DirectGateway {
    pub fn new(config: GatewayConfig, backend: Arc<dyn OneCliBackend>) -> Self {
        Self { config, backend }
    }
}

#[async_trait]
impl GatewayBackend for DirectGateway {
    fn gateway_type(&self) -> GatewayType {
        GatewayType::Direct
    }

    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, BackendError> {
        // Extract parameters to pass to the underlying backend
        // Note: Backend uses the old parameter-based API
        self.backend.chat_completion(
            request.model,
            request.messages,
            request.stream.unwrap_or(false),
            request.tools,
            request.tool_choice,
        ).await
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        self.backend.list_models().await
    }

    fn config(&self) -> &GatewayConfig {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// Traceloop Gateway Implementation
// ---------------------------------------------------------------------------

// TODO: Implement TraceloopGateway when traceloop module is fully integrated
// #[cfg(feature = "traceloop")]
// #[derive(Debug)]
// pub struct TraceloopGateway {
//     config: GatewayConfig,
//     router: crate::proxy::traceloop::TraceloopRouter,
// }
// 
// #[cfg(feature = "traceloop")]
// #[async_trait]
// impl GatewayBackend for TraceloopGateway {
//     fn gateway_type(&self) -> GatewayType {
//         GatewayType::Traceloop
//     }
// 
//     async fn chat_completion(
//         &self,
//         model: String,
//         messages: Vec<ChatMessage>,
//         stream: bool,
//         tools: Option<Vec<ToolDefinition>>,
//         tool_choice: Option<ToolChoice>,
//     ) -> Result<ChatCompletionResponse, BackendError> {
//         self.router.chat_completion(model, messages, stream, tools, tool_choice).await
//     }
// 
//     async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
//         self.router.list_models().await
//     }
// 
//     fn config(&self) -> &GatewayConfig {
//         &self.config
//     }
// }

// ---------------------------------------------------------------------------
// Mock Gateway Implementation (for testing)
// ---------------------------------------------------------------------------

#[cfg(feature = "test")]
#[derive(Debug)]
pub struct MockGateway {
    config: GatewayConfig,
}

#[cfg(feature = "test")]
impl MockGateway {
    pub fn new(config: GatewayConfig) -> Self {
        Self { config }
    }
}

#[cfg(feature = "test")]
#[async_trait]
impl GatewayBackend for MockGateway {
    fn gateway_type(&self) -> GatewayType {
        GatewayType::Mock
    }

    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, BackendError> {
        // Return a mock response for testing
        Ok(ChatCompletionResponse {
            id: "mock-id".to_string(),
            object: "chat.completion".to_string(),
            created: chrono::Utc::now().timestamp() as u64,
            model: request.model,
            choices: vec![],
            usage: Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            },
            system_fingerprint: None,
        })
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        // Return mock models
        Ok(vec![
            ModelInfo {
                id: "mock-model-1".to_string(),
                name: Some("Mock Model 1".to_string()),
                provider: Some("mock".to_string()),
                capabilities: vec!["chat".to_string(), "completion".to_string()],
            },
            ModelInfo {
                id: "mock-model-2".to_string(),
                name: Some("Mock Model 2".to_string()),
                provider: Some("mock".to_string()),
                capabilities: vec!["chat".to_string()],
            },
        ])
    }

    fn config(&self) -> &GatewayConfig {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gateway_type_parsing() {
        assert_eq!("helicone".parse::<GatewayType>().unwrap(), GatewayType::Helicone);
        assert_eq!("traceloop".parse::<GatewayType>().unwrap(), GatewayType::Traceloop);
        assert_eq!("direct".parse::<GatewayType>().unwrap(), GatewayType::Direct);
        assert_eq!("mock".parse::<GatewayType>().unwrap(), GatewayType::Mock);
        assert!("unknown".parse::<GatewayType>().is_err());
    }

    #[test]
    fn test_gateway_type_display() {
        assert_eq!(GatewayType::Helicone.to_string(), "helicone");
        assert_eq!(GatewayType::Traceloop.to_string(), "traceloop");
        assert_eq!(GatewayType::Direct.to_string(), "direct");
        assert_eq!(GatewayType::Mock.to_string(), "mock");
    }

    #[test]
    fn test_mock_gateway() {
        let config = GatewayConfig {
            backend_type: GatewayType::Mock,
            base_url: None,
            api_key: None,
            timeout_seconds: 30,
            extra_config: None,
        };

        let gateway = MockGateway::new(config);
        assert_eq!(gateway.gateway_type(), GatewayType::Mock);
    }
}
