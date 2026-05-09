//! GatewayBackend trait for abstracting different LLM gateway implementations.
//!
//! This module provides a unified interface for gateway engines. The shipped
//! root gateway engines are builtin HTTP, Helicone, and Mock.
//!
//! Each backend can be enabled via feature flags and selected via configuration.

use async_trait::async_trait;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use crate::config::GatewayRetryConfig;
use crate::proxy::backend::{BackendError, ModelInfo, SecretsBackend};
use crate::proxy::openai::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, Choice, MessageContent, Usage,
};

/// High-level capability flags used to compare builtin and external gateway
/// engines without committing Calciforge to one implementation.
#[derive(Debug, Clone, Default, serde::Serialize, PartialEq, Eq)]
pub struct GatewayCapabilities {
    pub openai_chat_completions: bool,
    pub model_listing: bool,
    pub tool_call_transcripts: bool,
    pub config_validation: bool,
    pub observability: bool,
    pub operator_ui: bool,
}

/// Operator-facing metadata for the active gateway engine.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct GatewayEngineInfo {
    pub id: String,
    pub display_name: String,
    pub ui_url: Option<String>,
    pub capabilities: GatewayCapabilities,
}

/// Configuration for a gateway backend
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// Type of gateway backend
    pub backend_type: GatewayType,
    /// Base URL for the gateway (if applicable)
    #[allow(dead_code)]
    pub base_url: Option<String>,
    /// API key for the gateway (if applicable)
    #[allow(dead_code)]
    pub api_key: Option<String>,
    /// Timeout in seconds
    #[allow(dead_code)]
    pub timeout_seconds: u64,
    /// Additional configuration as JSON
    #[allow(dead_code)]
    pub extra_config: Option<serde_json::Value>,

    /// Custom headers to include in requests
    #[allow(dead_code)]
    pub headers: Option<std::collections::HashMap<String, String>>,

    /// Retry policy for each concrete gateway attempt.
    pub retry: GatewayRetryConfig,

    /// Optional operator UI or dashboard URL for this gateway engine.
    pub ui_url: Option<String>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            backend_type: GatewayType::BuiltinHttp,
            base_url: None,
            api_key: None,
            timeout_seconds: 30,
            extra_config: None,
            headers: None,
            retry: GatewayRetryConfig::default(),
            ui_url: None,
        }
    }
}

/// Type of gateway backend
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayType {
    /// Helicone AI Gateway (HTTP-based)
    Helicone,
    /// Calciforge's minimal builtin OpenAI-compatible HTTP upstream adapter.
    BuiltinHttp,
    /// Mock gateway for testing
    Mock,
}

impl std::str::FromStr for GatewayType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "helicone" => Ok(GatewayType::Helicone),
            "http" | "builtin-http" | "builtin_http" | "direct" => Ok(GatewayType::BuiltinHttp),
            "mock" => Ok(GatewayType::Mock),
            _ => Err(format!("Unknown gateway type: {}", s)),
        }
    }
}

impl std::fmt::Display for GatewayType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GatewayType::Helicone => write!(f, "helicone"),
            GatewayType::BuiltinHttp => write!(f, "builtin-http"),
            GatewayType::Mock => write!(f, "mock"),
        }
    }
}

impl GatewayType {
    pub fn display_name(self) -> &'static str {
        match self {
            GatewayType::Helicone => "Helicone AI Gateway",
            GatewayType::BuiltinHttp => "Calciforge builtin HTTP upstream adapter",
            GatewayType::Mock => "Mock gateway",
        }
    }

    pub fn default_capabilities(self) -> GatewayCapabilities {
        match self {
            GatewayType::Helicone => GatewayCapabilities {
                openai_chat_completions: true,
                model_listing: false,
                tool_call_transcripts: false,
                config_validation: false,
                observability: true,
                operator_ui: true,
            },
            GatewayType::BuiltinHttp => GatewayCapabilities {
                openai_chat_completions: true,
                model_listing: true,
                tool_call_transcripts: false,
                config_validation: false,
                observability: false,
                operator_ui: false,
            },
            GatewayType::Mock => GatewayCapabilities {
                openai_chat_completions: true,
                model_listing: true,
                tool_call_transcripts: false,
                config_validation: false,
                observability: false,
                operator_ui: false,
            },
        }
    }
}

impl GatewayConfig {
    pub fn engine_info(&self, gateway_type: GatewayType) -> GatewayEngineInfo {
        GatewayEngineInfo {
            id: gateway_type.to_string(),
            display_name: gateway_type.display_name().to_string(),
            ui_url: self.ui_url.clone(),
            capabilities: gateway_type.default_capabilities(),
        }
    }
}

/// Main trait for gateway backends
#[async_trait]
#[allow(dead_code)]
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

    /// Return operator-facing engine metadata. External gateway spikes should
    /// make this accurate before becoming supported options.
    fn engine_info(&self) -> GatewayEngineInfo {
        self.config().engine_info(self.gateway_type())
    }
}

/// Create a gateway backend from configuration
pub fn create_gateway(
    config: GatewayConfig,
    backend: Option<Arc<dyn SecretsBackend>>,
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
                headers: config.headers.clone().unwrap_or_default(),
                retry: config.retry.clone(),
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

        GatewayType::Mock => {
            let inner_gateway = Arc::new(MockGateway::new(config.clone()));

            // Wrap with logging for debugging
            Ok(Arc::new(LoggingGateway::new(config, inner_gateway)))
        }

        GatewayType::BuiltinHttp => {
            // Builtin HTTP upstream calls
            // This requires a backend to be passed in
            let backend = backend.ok_or_else(|| {
                BackendError::ConfigError(
                    "Builtin HTTP gateway requires a backend parameter".to_string(),
                )
            })?;

            let inner_gateway = Arc::new(BuiltinHttpGateway::new(config.clone(), backend));

            // Wrap with logging for debugging
            Ok(Arc::new(LoggingGateway::new(config, inner_gateway)))
        }

        #[cfg(not(feature = "helicone"))]
        GatewayType::Helicone => Err(BackendError::ConfigError(
            "Helicone feature not enabled".to_string(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Helicone Gateway Implementation
// ---------------------------------------------------------------------------

#[cfg(feature = "helicone")]
#[derive(Debug)]
#[allow(dead_code)]
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
        self.router.chat_completion_request(request).await
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
#[allow(dead_code)]
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
        self.inner.gateway_type()
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

        let mut attempt = 0_u32;
        let mut result = loop {
            let result = self.inner.chat_completion(request.clone()).await;
            match result {
                Ok(response) => {
                    info!("Request succeeded");
                    break Ok(response);
                }
                Err(error) => {
                    let failure_kind = error.failure_kind();
                    let should_retry = should_retry_locally(
                        self.inner.gateway_type(),
                        &self.config.retry,
                        &error,
                        attempt,
                    );
                    warn!(
                        error = %error,
                        ?failure_kind,
                        attempt = attempt + 1,
                        max_retries = self.config.retry.max_retries,
                        should_retry,
                        "Gateway request failed"
                    );
                    if !should_retry {
                        break Err(error);
                    }
                    let delay = retry_delay(&self.config.retry, attempt);
                    attempt += 1;
                    tokio::time::sleep(delay).await;
                }
            }
        };

        let duration = start.elapsed();

        // Normalize response model back to client format
        if let Ok(ref mut response) = result
            && response.model.starts_with("kimi-")
        {
            let prefixed = format!("kimi/{}", response.model);
            info!(
                "Normalizing response model: {} -> {}",
                response.model, prefixed
            );
            response.model = prefixed;
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

    fn engine_info(&self) -> GatewayEngineInfo {
        self.inner.engine_info()
    }
}

fn should_retry_locally(
    gateway_type: GatewayType,
    policy: &GatewayRetryConfig,
    error: &BackendError,
    attempt: u32,
) -> bool {
    if gateway_type == GatewayType::Helicone {
        // Helicone retry policy is passed to the gateway engine as request
        // headers. Retrying again here would multiply attempts and costs.
        return false;
    }

    policy.enabled
        && attempt < policy.max_retries
        && policy.retry_on.contains(&error.failure_kind())
}

fn retry_delay(policy: &GatewayRetryConfig, attempt: u32) -> Duration {
    let factor = policy.factor.max(1) as u128;
    let multiplier = factor.saturating_pow(attempt);
    let base_delay = (policy.min_timeout_ms as u128)
        .saturating_mul(multiplier)
        .min(policy.max_timeout_ms as u128);
    let jitter_percent = rand::random_range(80_u128..=120_u128);
    let delay = base_delay
        .saturating_mul(jitter_percent)
        .saturating_div(100)
        .min(policy.max_timeout_ms as u128);
    Duration::from_millis(delay as u64)
}

// ---------------------------------------------------------------------------
// Builtin HTTP Gateway Implementation (wraps existing SecretsBackend)
// ---------------------------------------------------------------------------

/// Builtin HTTP gateway that wraps an existing SecretsBackend
pub struct BuiltinHttpGateway {
    config: GatewayConfig,
    backend: Arc<dyn SecretsBackend>,
}

impl Debug for BuiltinHttpGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltinHttpGateway")
            .field("config", &self.config)
            .field("backend_type", &self.backend.backend_type())
            .finish()
    }
}

impl BuiltinHttpGateway {
    pub fn new(config: GatewayConfig, backend: Arc<dyn SecretsBackend>) -> Self {
        Self { config, backend }
    }
}

#[async_trait]
impl GatewayBackend for BuiltinHttpGateway {
    fn gateway_type(&self) -> GatewayType {
        GatewayType::BuiltinHttp
    }

    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, BackendError> {
        self.backend.chat_completion_request(request).await
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        self.backend.list_models().await
    }

    fn config(&self) -> &GatewayConfig {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// Mock Gateway Implementation (for testing)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct MockGateway {
    config: GatewayConfig,
}

impl MockGateway {
    pub fn new(config: GatewayConfig) -> Self {
        Self { config }
    }
}

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
            model: request.model.clone(),
            choices: vec![Choice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(MessageContent::Text(format!(
                        "Mock gateway response for model: {}",
                        request.model
                    ))),
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
            "direct".parse::<GatewayType>().unwrap(),
            GatewayType::BuiltinHttp
        );
        assert_eq!(
            "http".parse::<GatewayType>().unwrap(),
            GatewayType::BuiltinHttp
        );
        assert_eq!(
            "builtin-http".parse::<GatewayType>().unwrap(),
            GatewayType::BuiltinHttp
        );
        assert_eq!("mock".parse::<GatewayType>().unwrap(), GatewayType::Mock);
        assert!("unknown".parse::<GatewayType>().is_err());
    }

    #[test]
    fn test_gateway_type_display() {
        assert_eq!(GatewayType::Helicone.to_string(), "helicone");
        assert_eq!(GatewayType::BuiltinHttp.to_string(), "builtin-http");
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
            retry: GatewayRetryConfig::default(),
            ui_url: None,
        };

        let gateway = MockGateway::new(config);
        assert_eq!(gateway.gateway_type(), GatewayType::Mock);
    }

    #[tokio::test]
    async fn mock_gateway_returns_openai_compatible_chat_choice() {
        let gateway = MockGateway::new(GatewayConfig {
            backend_type: GatewayType::Mock,
            ..Default::default()
        });

        let response = gateway
            .chat_completion(ChatCompletionRequest {
                model: "gpt-4".to_string(),
                messages: vec![ChatMessage {
                    role: "user".to_string(),
                    content: Some(MessageContent::Text("short".to_string())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning: None,
                    reasoning_content: None,
                }],
                max_tokens: Some(2),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(response.model, "gpt-4");
        let choice = response
            .choices
            .first()
            .expect("mock gateway should return an assistant choice");
        assert_eq!(choice.message.role, "assistant");
        let Some(MessageContent::Text(content)) = choice.message.content.as_ref() else {
            panic!("mock gateway choice should contain text content");
        };
        assert!(
            content.contains("gpt-4") && content.to_lowercase().contains("mock"),
            "mock response content should identify the routed model: {content}"
        );
    }

    #[test]
    fn gateway_engine_info_carries_operator_ui_link() {
        let config = GatewayConfig {
            backend_type: GatewayType::Helicone,
            ui_url: Some("http://127.0.0.1:8585".to_string()),
            ..Default::default()
        };

        let info = config.engine_info(GatewayType::Helicone);

        assert_eq!(info.id, "helicone");
        assert_eq!(info.display_name, "Helicone AI Gateway");
        assert_eq!(info.ui_url.as_deref(), Some("http://127.0.0.1:8585"));
        assert!(info.capabilities.operator_ui);
        assert!(info.capabilities.observability);
        assert!(!info.capabilities.model_listing);
        assert!(!info.capabilities.tool_call_transcripts);
        assert!(!info.capabilities.config_validation);
    }

    #[test]
    fn builtin_http_gateway_retries_only_configured_failure_kinds() {
        let retry = GatewayRetryConfig {
            enabled: true,
            max_retries: 2,
            min_timeout_ms: 1,
            max_timeout_ms: 10,
            factor: 2,
            retry_on: vec![crate::config::GatewayFailureKind::ServerError],
        };
        let server_error =
            BackendError::http_status_error(reqwest::StatusCode::SERVICE_UNAVAILABLE, "down");
        let auth_error =
            BackendError::http_status_error(reqwest::StatusCode::UNAUTHORIZED, "bad key");

        assert!(should_retry_locally(
            GatewayType::BuiltinHttp,
            &retry,
            &server_error,
            0
        ));
        assert!(!should_retry_locally(
            GatewayType::BuiltinHttp,
            &retry,
            &auth_error,
            0
        ));
        assert!(!should_retry_locally(
            GatewayType::BuiltinHttp,
            &retry,
            &server_error,
            2
        ));
    }

    #[test]
    fn helicone_retry_policy_is_not_applied_twice_locally() {
        let retry = GatewayRetryConfig {
            enabled: true,
            max_retries: 2,
            min_timeout_ms: 1,
            max_timeout_ms: 10,
            factor: 2,
            retry_on: vec![crate::config::GatewayFailureKind::ServerError],
        };
        let server_error =
            BackendError::http_status_error(reqwest::StatusCode::SERVICE_UNAVAILABLE, "down");

        assert!(
            !should_retry_locally(GatewayType::Helicone, &retry, &server_error, 0),
            "Helicone receives retry headers, so Calciforge must not multiply attempts locally"
        );
    }

    #[tokio::test]
    async fn builtin_http_gateway_forwards_complete_chat_request_options() {
        use crate::proxy::backend::{BackendConfig, BackendType, create_backend};
        use crate::proxy::openai::{ChatMessage, Choice, MessageContent, Usage};
        use mockito::Matcher;
        use std::collections::HashMap;

        let mut server = mockito::Server::new_async().await;
        let response = ChatCompletionResponse {
            id: "chatcmpl-test".to_string(),
            object: "chat.completion".to_string(),
            created: 1,
            model: "kimi-for-coding".to_string(),
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
            .match_header("x-client-family", "kimi-cli")
            .match_body(Matcher::PartialJson(serde_json::json!({
                "model": "kimi-for-coding",
                "max_tokens": 16,
                "temperature": 0.5,
                "thinking": {"type": "enabled"},
                "stream": false,
                "messages": [{"role": "user", "content": "hello"}]
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(serde_json::to_string(&response).unwrap())
            .create_async()
            .await;

        let mut headers = HashMap::new();
        headers.insert("x-client-family".to_string(), "kimi-cli".to_string());
        let backend = create_backend(&BackendConfig {
            backend_type: BackendType::Http,
            url: Some(format!("{}/v1", server.url())),
            api_key: Some("provider-key".to_string()),
            timeout_seconds: Some(30),
            headers: Some(headers.clone()),
            ..Default::default()
        })
        .unwrap();
        let gateway = create_gateway(
            GatewayConfig {
                backend_type: GatewayType::BuiltinHttp,
                base_url: Some(format!("{}/v1", server.url())),
                api_key: Some("provider-key".to_string()),
                timeout_seconds: 30,
                headers: Some(headers),
                ..Default::default()
            },
            Some(backend),
        )
        .unwrap();

        let result = gateway
            .chat_completion(
                serde_json::from_value(serde_json::json!({
                    "model": "kimi-for-coding",
                    "messages": [{"role": "user", "content": "hello"}],
                    "max_tokens": 16,
                    "temperature": 0.5,
                    "thinking": {"type": "enabled"}
                }))
                .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(result.model, "kimi/kimi-for-coding");
        mock.assert_async().await;
    }

    #[cfg(feature = "helicone")]
    #[test]
    fn create_helicone_gateway_preserves_engine_metadata_through_logging_wrapper() {
        let gateway = create_gateway(
            GatewayConfig {
                backend_type: GatewayType::Helicone,
                base_url: Some("https://ai-gateway.helicone.ai".to_string()),
                api_key: Some("helicone-test-key".to_string()),
                ui_url: Some("https://us.helicone.ai/requests".to_string()),
                ..Default::default()
            },
            None,
        )
        .unwrap();

        let info = gateway.engine_info();

        assert_eq!(gateway.gateway_type(), GatewayType::Helicone);
        assert_eq!(info.id, "helicone");
        assert_eq!(info.display_name, "Helicone AI Gateway");
        assert_eq!(
            info.ui_url.as_deref(),
            Some("https://us.helicone.ai/requests")
        );
        assert!(info.capabilities.openai_chat_completions);
        assert!(info.capabilities.operator_ui);
        assert!(info.capabilities.observability);
    }

    #[cfg(feature = "helicone")]
    #[tokio::test]
    async fn helicone_gateway_forwards_complete_chat_request_options() {
        use crate::proxy::openai::{ChatMessage, Choice, MessageContent, Usage};
        use mockito::Matcher;

        let mut server = mockito::Server::new_async().await;
        let response = ChatCompletionResponse {
            id: "chatcmpl-test".to_string(),
            object: "chat.completion".to_string(),
            created: 1,
            model: "ollama/qwen3.6:27b".to_string(),
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
            .match_body(Matcher::PartialJson(serde_json::json!({
                "model": "ollama/qwen3.6:27b",
                "max_tokens": 16,
                "temperature": 0.2,
                "stream": false,
                "messages": [{"role": "user", "content": "hello"}]
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(serde_json::to_string(&response).unwrap())
            .create_async()
            .await;

        let gateway = create_gateway(
            GatewayConfig {
                backend_type: GatewayType::Helicone,
                base_url: Some(format!("{}/v1/", server.url())),
                api_key: Some("helicone-test-key".to_string()),
                timeout_seconds: 30,
                ..Default::default()
            },
            None,
        )
        .unwrap();

        let result = gateway
            .chat_completion(ChatCompletionRequest {
                model: "ollama/qwen3.6:27b".to_string(),
                messages: vec![ChatMessage {
                    role: "user".to_string(),
                    content: Some(MessageContent::Text("hello".to_string())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning: None,
                    reasoning_content: None,
                }],
                max_tokens: Some(16),
                temperature: Some(0.2),
                stream: Some(false),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.model, "ollama/qwen3.6:27b");
        mock.assert_async().await;
    }
}
