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
use backon::Retryable;
use std::fmt::Debug;
use std::sync::Arc;

use crate::proxy::backend::{BackendError, ModelInfo, OneCliBackend};
use crate::proxy::openai::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, ToolChoice, ToolDefinition, Usage,
};
use tracing::{info, warn};

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

    /// Custom headers to include in requests
    pub headers: Option<std::collections::HashMap<String, String>>,

    /// Enable retry logic (default: true)
    pub retry_enabled: bool,

    /// Maximum number of retries (default: 3)
    pub max_retries: u32,

    /// Base delay between retries in milliseconds (default: 1000)
    pub retry_base_delay_ms: u64,

    /// Maximum delay between retries in milliseconds (default: 10000)
    pub retry_max_delay_ms: u64,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            backend_type: GatewayType::Direct,
            base_url: None,
            api_key: None,
            timeout_seconds: 30,
            extra_config: None,
            headers: None,
            retry_enabled: true,
            max_retries: 3,
            retry_base_delay_ms: 1000,
            retry_max_delay_ms: 10000,
        }
    }
}

// Removed with_retry helper for now - causing compilation issues

// Removed RetryGatewayDyn for now - causing compilation issues
// We'll implement retry properly later

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
) -> Result<Arc<dyn GatewayBackend>, BackendError> {
    match config.backend_type {
        #[cfg(feature = "helicone")]
        GatewayType::Helicone => {
            use crate::proxy::helicone_router::{HeliconeRouter, HeliconeRouterConfig};

            let helicone_config = HeliconeRouterConfig {
                base_url: config
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "http://localhost:8787".to_string()),
                api_key: config.api_key.clone().unwrap_or_default(),
                timeout_seconds: config.timeout_seconds,
                router_name: "helicone".to_string(),
                enable_caching: false,
                cache_ttl_seconds: 300,
            };

            let router = HeliconeRouter::new(helicone_config).map_err(|e| {
                BackendError::ConfigError(format!("Failed to create Helicone router: {}", e))
            })?;

            let inner_gateway = Arc::new(HeliconeGateway {
                config: config.clone(),
                router,
            });

            // Wrap with logging for debugging
            Ok(Arc::new(LoggingGateway::new(config, inner_gateway)))
        }

        #[cfg(feature = "traceloop")]
        GatewayType::Traceloop => {
            use crate::proxy::traceloop::{ProviderConfig, ProviderType, TraceloopRouter};

            // Create provider configurations from config
            // Use the actual API key and URL from config
            let providers = vec![ProviderConfig {
                id: "kimi".to_string(),
                r#type: ProviderType::Kimi,
                api_key: config.api_key.clone().unwrap_or_default(),
                base_url: config.base_url.clone(),
                default_model: "kimi-for-coding".to_string(),
            }];

            let router = TraceloopRouter::new(providers).map_err(|e| {
                BackendError::ConfigError(format!("Failed to create Traceloop router: {}", e))
            })?;

            let inner_gateway = Arc::new(TraceloopGateway {
                config: config.clone(),
                router,
            });

            // Wrap with logging for debugging
            Ok(Arc::new(LoggingGateway::new(config, inner_gateway)))
        }

        #[cfg(feature = "test")]
        GatewayType::Mock => {
            let inner_gateway = Arc::new(MockGateway::new(config.clone()));

            // Wrap with logging for debugging
            Ok(Arc::new(LoggingGateway::new(config, inner_gateway)))
        }

        #[cfg(not(feature = "test"))]
        GatewayType::Mock => Err(BackendError::ConfigError(
            "Mock gateway only available in test mode".to_string(),
        )),

        GatewayType::Direct => {
            // Direct provider calls (no gateway)
            // This requires a backend to be passed in
            let backend = backend.ok_or_else(|| {
                BackendError::ConfigError("Direct gateway requires a backend parameter".to_string())
            })?;

            let inner_gateway = Arc::new(DirectGateway::new(config.clone(), backend));

            // Wrap with logging for debugging
            Ok(Arc::new(LoggingGateway::new(config, inner_gateway)))
        }

        #[cfg(not(feature = "helicone"))]
        GatewayType::Helicone => Err(BackendError::ConfigError(
            "Helicone feature not enabled".to_string(),
        )),

        #[cfg(not(feature = "traceloop"))]
        GatewayType::Traceloop => Err(BackendError::ConfigError(
            "Traceloop feature not enabled".to_string(),
        )),
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
        self.router
            .chat_completion(
                request.model,
                request.messages,
                request.stream.unwrap_or(false),
                request.tools,
                request.tool_choice,
            )
            .await
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        self.router.list_models().await
    }

    fn config(&self) -> &GatewayConfig {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// Logging Gateway (wraps another gateway for debugging)
// ---------------------------------------------------------------------------

/// Logging gateway that wraps another gateway and logs all requests
#[derive(Debug)]
pub struct LoggingGateway {
    config: GatewayConfig,
    inner: Arc<dyn GatewayBackend>,
}

impl LoggingGateway {
    pub fn new(config: GatewayConfig, inner: Arc<dyn GatewayBackend>) -> Self {
        Self { config, inner }
    }
}

#[async_trait]
impl GatewayBackend for LoggingGateway {
    fn gateway_type(&self) -> GatewayType {
        GatewayType::Direct // Same as inner
    }

    async fn chat_completion(
        &self,
        mut request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, BackendError> {
        use tracing::{info, warn};

        // Normalize model name for Kimi API
        if request.model.starts_with("kimi/") {
            let stripped = request.model.trim_start_matches("kimi/");
            info!(
                "Normalizing request model: {} -> {}",
                request.model, stripped
            );
            request.model = stripped.to_string();
        }

        info!(
            "Gateway request: model={}, messages={}, stream={}, tools={:?}",
            request.model,
            request.messages.len(),
            request.stream.unwrap_or(false),
            request.tools.is_some()
        );

        let start = std::time::Instant::now();

        // Apply retry logic
        let inner = self.inner.clone();
        let request_clone = request.clone();

        // Simple retry logic using backon
        let operation = || async {
            let result = inner.chat_completion(request_clone.clone()).await;

            // Check if we should retry
            match &result {
                Ok(_) => {
                    info!("Request succeeded");
                    result
                }
                Err(e) => {
                    // Retry on HTTP errors (5xx) and rate limits (429)
                    let error_str = e.to_string();
                    let should_retry = error_str.contains("500")
                        || error_str.contains("502")
                        || error_str.contains("503")
                        || error_str.contains("504")
                        || error_str.contains("429")
                        || error_str.contains("timeout")
                        || error_str.contains("network");

                    if should_retry {
                        warn!("Retryable error: {}", e);
                    } else {
                        warn!("Non-retryable error: {}", e);
                    }

                    result
                }
            }
        };

        // Exponential backoff: 1s, 2s, 4s, 8s with jitter
        let mut result = operation
            .retry(
                &backon::ExponentialBuilder::default()
                    .with_min_delay(std::time::Duration::from_secs(1))
                    .with_max_delay(std::time::Duration::from_secs(8))
                    .with_max_times(3)
                    .with_factor(2.0)
                    .with_jitter(),
            )
            .await;

        let duration = start.elapsed();

        // Normalize response model back to client format
        if let Ok(ref mut response) = result {
            if response.model.starts_with("kimi-") {
                let prefixed = format!("kimi/{}", response.model);
                info!(
                    "Normalizing response model: {} -> {}",
                    response.model, prefixed
                );
                response.model = prefixed;
            }
        }

        match &result {
            Ok(response) => {
                info!(
                    "Gateway response: id={}, model={}, duration={:?}, choices={}",
                    response.id,
                    response.model,
                    duration,
                    response.choices.len()
                );
            }
            Err(e) => {
                warn!("Gateway error: {}, duration={:?}", e, duration);
            }
        }

        result
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        use tracing::info;

        info!("Gateway list_models request");
        let result = self.inner.list_models().await;

        match &result {
            Ok(models) => {
                info!("Gateway list_models response: {} models", models.len());

                // Normalize model names for consistency
                let normalized_models: Vec<ModelInfo> = models
                    .iter()
                    .map(|model| {
                        let mut normalized = model.clone();

                        // Map Kimi models: kimi-for-coding -> kimi/kimi-for-coding
                        // Also handle kimi-free, kimi-pro, etc.
                        if normalized.id.starts_with("kimi-") {
                            let prefixed = format!("kimi/{}", normalized.id);
                            info!("Normalizing model name: {} -> {}", normalized.id, prefixed);
                            normalized.id = prefixed;
                        }

                        normalized
                    })
                    .collect();

                for model in &normalized_models {
                    info!(
                        "  - {} ({})",
                        model.id,
                        model.provider.as_deref().unwrap_or("unknown")
                    );
                }

                return Ok(normalized_models);
            }
            Err(e) => {
                info!("Gateway list_models error: {}", e);
            }
        }

        result
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
        self.backend
            .chat_completion(
                request.model,
                request.messages,
                request.stream.unwrap_or(false),
                request.tools,
                request.tool_choice,
            )
            .await
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

// Traceloop Gateway Implementation
#[cfg(feature = "traceloop")]
#[derive(Debug)]
pub struct TraceloopGateway {
    config: GatewayConfig,
    router: crate::proxy::traceloop::TraceloopRouter,
}

#[cfg(feature = "traceloop")]
#[async_trait]
impl GatewayBackend for TraceloopGateway {
    fn gateway_type(&self) -> GatewayType {
        GatewayType::Traceloop
    }

    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, BackendError> {
        self.router
            .chat_completion(
                request.model,
                request.messages,
                request.stream.unwrap_or(false),
                request.tools,
                request.tool_choice,
            )
            .await
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        self.router.list_models().await
    }

    fn config(&self) -> &GatewayConfig {
        &self.config
    }
}

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
        assert_eq!(
            "helicone".parse::<GatewayType>().unwrap(),
            GatewayType::Helicone
        );
        assert_eq!(
            "traceloop".parse::<GatewayType>().unwrap(),
            GatewayType::Traceloop
        );
        assert_eq!(
            "direct".parse::<GatewayType>().unwrap(),
            GatewayType::Direct
        );
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
        use super::MockGateway;
        
        let config = GatewayConfig {
            backend_type: GatewayType::Mock,
            base_url: None,
            api_key: None,
            timeout_seconds: 30,
            extra_config: None,
            headers: None,
            retry_enabled: true,
            max_retries: 3,
            retry_base_delay_ms: 1000,
            retry_max_delay_ms: 10000,
        };

        let gateway = MockGateway::new(config);
        assert_eq!(gateway.gateway_type(), GatewayType::Mock);
    }
}
