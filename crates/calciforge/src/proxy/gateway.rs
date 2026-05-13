//! ProviderAdapter trait for model-provider boundaries.
//!
//! Calciforge should not assume there is one installed "model gateway". It owns
//! a policy/audit/auth boundary, then routes to one or more configured provider
//! adapters such as builtin OpenAI-compatible HTTP, Helicone, LiteLLM,
//! OpenRouter, Ollama, or mock test adapters.
//!
//! The older config field names still say `backend_type` for compatibility, but
//! runtime code should treat these as adapter kinds.

use async_trait::async_trait;
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use crate::config::GatewayRetryConfig;
use crate::proxy::backend::{BackendError, ModelInfo, SecretsBackend};
use crate::proxy::openai::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, Choice, MessageContent, Usage,
};

/// High-level capability flags used to compare builtin and external provider
/// adapters without committing Calciforge to one implementation.
#[derive(Debug, Clone, Default, serde::Serialize, PartialEq, Eq)]
pub struct GatewayCapabilities {
    pub openai_chat_completions: bool,
    pub model_listing: bool,
    pub tool_call_transcripts: bool,
    pub config_validation: bool,
    pub observability: bool,
    pub operator_ui: bool,
}

/// Operator-facing metadata for a provider adapter.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct GatewayEngineInfo {
    pub id: String,
    pub display_name: String,
    pub ui_url: Option<String>,
    pub capabilities: GatewayCapabilities,
    pub observability: Vec<ProviderObservabilityCapability>,
}

/// Observability sink kinds a provider adapter can expose without forcing the
/// adapter to become the model request path.
#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderObservabilityKind {
    NativeDashboard,
    OTel,
    OpenInference,
    Langfuse,
}

/// A concrete observability surface supported by a provider adapter.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct ProviderObservabilityCapability {
    pub kind: ProviderObservabilityKind,
    pub display_name: String,
    pub endpoint_required: bool,
}

impl ProviderObservabilityCapability {
    pub fn new(
        kind: ProviderObservabilityKind,
        display_name: impl Into<String>,
        endpoint_required: bool,
    ) -> Self {
        Self {
            kind,
            display_name: display_name.into(),
            endpoint_required,
        }
    }
}

/// Configuration for a provider adapter.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// Type of provider adapter
    pub backend_type: GatewayType,
    /// Base URL for the provider adapter (if applicable).
    #[allow(dead_code)]
    pub base_url: Option<String>,
    /// API key used by Calciforge to authenticate to the provider adapter.
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

    /// Retry policy for each concrete adapter attempt.
    pub retry: GatewayRetryConfig,

    /// Optional operator UI or dashboard URL for this provider adapter.
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

/// Type of provider adapter
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayType {
    /// Helicone AI Gateway (HTTP-based).
    Helicone,
    /// Calciforge's minimal builtin OpenAI-compatible HTTP upstream adapter.
    BuiltinHttp,
    /// LiteLLM OpenAI-compatible proxy/gateway.
    LiteLlm,
    /// Portkey OpenAI-compatible gateway.
    Portkey,
    /// TensorZero OpenAI-compatible gateway.
    TensorZero,
    /// Future AGI OpenAI-compatible gateway.
    FutureAgi,
    /// OpenRouter OpenAI-compatible provider boundary.
    OpenRouter,
    /// Mock adapter for tests only.
    Mock,
}

impl std::str::FromStr for GatewayType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "helicone" => Ok(GatewayType::Helicone),
            "http" | "builtin-http" | "builtin_http" | "direct" => Ok(GatewayType::BuiltinHttp),
            "litellm" | "lite-llm" | "lite_llm" => Ok(GatewayType::LiteLlm),
            "portkey" => Ok(GatewayType::Portkey),
            "tensorzero" | "tensor-zero" | "tensor_zero" => Ok(GatewayType::TensorZero),
            "future-agi" | "future_agi" | "futureagi" => Ok(GatewayType::FutureAgi),
            "openrouter" | "open-router" | "open_router" => Ok(GatewayType::OpenRouter),
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
            GatewayType::LiteLlm => write!(f, "litellm"),
            GatewayType::Portkey => write!(f, "portkey"),
            GatewayType::TensorZero => write!(f, "tensorzero"),
            GatewayType::FutureAgi => write!(f, "future-agi"),
            GatewayType::OpenRouter => write!(f, "openrouter"),
            GatewayType::Mock => write!(f, "mock"),
        }
    }
}

impl GatewayType {
    pub const SUPPORTED_CONFIG_NAMES: &'static [&'static str] = &[
        "http",
        "helicone",
        "litellm",
        "portkey",
        "tensorzero",
        "future-agi",
        "openrouter",
        "mock",
    ];

    pub const SUPPORTED_PROVIDER_CONFIG_NAMES: &'static [&'static str] = &[
        "http",
        "helicone",
        "litellm",
        "portkey",
        "tensorzero",
        "future-agi",
        "openrouter",
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            GatewayType::Helicone => "Helicone AI Gateway",
            GatewayType::BuiltinHttp => "Calciforge builtin HTTP upstream adapter",
            GatewayType::LiteLlm => "LiteLLM gateway",
            GatewayType::Portkey => "Portkey gateway",
            GatewayType::TensorZero => "TensorZero gateway",
            GatewayType::FutureAgi => "Future AGI gateway",
            GatewayType::OpenRouter => "OpenRouter",
            GatewayType::Mock => "Mock provider adapter",
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
            GatewayType::LiteLlm => GatewayCapabilities {
                openai_chat_completions: true,
                model_listing: true,
                tool_call_transcripts: false,
                config_validation: false,
                observability: true,
                operator_ui: true,
            },
            GatewayType::Portkey => GatewayCapabilities {
                openai_chat_completions: true,
                model_listing: false,
                tool_call_transcripts: false,
                config_validation: false,
                observability: true,
                operator_ui: true,
            },
            GatewayType::TensorZero => GatewayCapabilities {
                openai_chat_completions: true,
                model_listing: false,
                tool_call_transcripts: false,
                config_validation: false,
                observability: true,
                operator_ui: true,
            },
            GatewayType::FutureAgi => GatewayCapabilities {
                openai_chat_completions: true,
                model_listing: false,
                tool_call_transcripts: false,
                config_validation: false,
                observability: true,
                operator_ui: true,
            },
            GatewayType::OpenRouter => GatewayCapabilities {
                openai_chat_completions: true,
                model_listing: true,
                tool_call_transcripts: false,
                config_validation: false,
                observability: false,
                operator_ui: true,
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

    pub fn observability_capabilities(self) -> Vec<ProviderObservabilityCapability> {
        match self {
            GatewayType::Helicone => vec![ProviderObservabilityCapability::new(
                ProviderObservabilityKind::NativeDashboard,
                "Helicone request dashboard",
                false,
            )],
            GatewayType::LiteLlm => vec![
                ProviderObservabilityCapability::new(
                    ProviderObservabilityKind::NativeDashboard,
                    "LiteLLM dashboard",
                    false,
                ),
                ProviderObservabilityCapability::new(
                    ProviderObservabilityKind::Langfuse,
                    "Langfuse callback",
                    true,
                ),
                ProviderObservabilityCapability::new(
                    ProviderObservabilityKind::OpenInference,
                    "OpenInference trace export",
                    true,
                ),
            ],
            GatewayType::Portkey => vec![
                ProviderObservabilityCapability::new(
                    ProviderObservabilityKind::NativeDashboard,
                    "Portkey request dashboard",
                    false,
                ),
                ProviderObservabilityCapability::new(
                    ProviderObservabilityKind::OTel,
                    "OpenTelemetry export",
                    true,
                ),
            ],
            GatewayType::TensorZero => vec![
                ProviderObservabilityCapability::new(
                    ProviderObservabilityKind::NativeDashboard,
                    "TensorZero observability UI",
                    false,
                ),
                ProviderObservabilityCapability::new(
                    ProviderObservabilityKind::OpenInference,
                    "OpenInference trace export",
                    true,
                ),
            ],
            GatewayType::FutureAgi => vec![
                ProviderObservabilityCapability::new(
                    ProviderObservabilityKind::NativeDashboard,
                    "Future AGI evaluation dashboard",
                    false,
                ),
                ProviderObservabilityCapability::new(
                    ProviderObservabilityKind::OTel,
                    "OpenTelemetry export",
                    true,
                ),
            ],
            GatewayType::BuiltinHttp | GatewayType::OpenRouter | GatewayType::Mock => Vec::new(),
        }
    }

    pub fn uses_openai_compatible_http_core(self) -> bool {
        matches!(
            self,
            GatewayType::BuiltinHttp
                | GatewayType::Helicone
                | GatewayType::LiteLlm
                | GatewayType::Portkey
                | GatewayType::TensorZero
                | GatewayType::FutureAgi
                | GatewayType::OpenRouter
        )
    }

    pub fn requires_backend_url(self) -> bool {
        !matches!(self, GatewayType::Mock)
    }

    pub fn delegates_retry_to_adapter(self) -> bool {
        matches!(self, GatewayType::Helicone)
    }
}

impl GatewayConfig {
    pub fn engine_info(&self, gateway_type: GatewayType) -> GatewayEngineInfo {
        GatewayEngineInfo {
            id: gateway_type.to_string(),
            display_name: gateway_type.display_name().to_string(),
            ui_url: self.ui_url.clone(),
            capabilities: gateway_type.default_capabilities(),
            observability: gateway_type.observability_capabilities(),
        }
    }
}

/// Apply engine-specific OpenAI-compatible HTTP headers while keeping all
/// engines on the same request/response core.
pub(crate) fn openai_compatible_headers(
    gateway_type: GatewayType,
    api_key: Option<&str>,
    retry: &GatewayRetryConfig,
    configured: Option<&HashMap<String, String>>,
) -> Option<HashMap<String, String>> {
    let mut headers = configured.cloned().unwrap_or_default();
    if gateway_type == GatewayType::Helicone {
        if let Some(key) = api_key.map(str::trim).filter(|key| !key.is_empty()) {
            headers.insert("helicone-auth".to_string(), format!("Bearer {key}"));
        }
        if retry.enabled {
            headers.insert("helicone-retry-enabled".to_string(), "true".to_string());
            headers.insert(
                "helicone-retry-num".to_string(),
                retry.max_retries.to_string(),
            );
            headers.insert(
                "helicone-retry-min-timeout".to_string(),
                retry.min_timeout_ms.to_string(),
            );
            headers.insert(
                "helicone-retry-max-timeout".to_string(),
                retry.max_timeout_ms.to_string(),
            );
            headers.insert(
                "helicone-retry-factor".to_string(),
                retry.factor.to_string(),
            );
        }
    }
    if headers.is_empty() {
        None
    } else {
        Some(headers)
    }
}

/// Main trait for provider adapters
#[async_trait]
#[allow(dead_code)]
pub trait ProviderAdapter: Send + Sync + Debug {
    /// Get the type of this provider adapter.
    fn gateway_type(&self) -> GatewayType;

    /// Make a chat completion request
    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, BackendError>;

    /// List available models
    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError>;

    /// Get provider adapter configuration.
    fn config(&self) -> &GatewayConfig;

    /// Return operator-facing adapter metadata.
    fn engine_info(&self) -> GatewayEngineInfo {
        self.config().engine_info(self.gateway_type())
    }

    /// Return observability surfaces the adapter knows how to expose.
    fn observability_capabilities(&self) -> Vec<ProviderObservabilityCapability> {
        self.gateway_type().observability_capabilities()
    }
}

/// Create a provider adapter from configuration
pub fn create_gateway(
    config: GatewayConfig,
    backend: Option<Arc<dyn SecretsBackend>>,
) -> Result<Arc<dyn ProviderAdapter>, BackendError> {
    match config.backend_type {
        GatewayType::Mock => {
            let inner_gateway = Arc::new(MockGateway::new(config.clone()));

            // Wrap with logging for debugging
            Ok(Arc::new(LoggingGateway::new(config, inner_gateway)))
        }

        GatewayType::Helicone
        | GatewayType::BuiltinHttp
        | GatewayType::LiteLlm
        | GatewayType::Portkey
        | GatewayType::TensorZero
        | GatewayType::FutureAgi
        | GatewayType::OpenRouter => {
            // Builtin HTTP upstream calls
            // This requires a backend to be passed in
            let backend = backend.ok_or_else(|| {
                BackendError::ConfigError(format!(
                    "{} gateway requires a backend parameter",
                    config.backend_type
                ))
            })?;

            let inner_gateway = Arc::new(BuiltinHttpGateway::new(config.clone(), backend));

            // Wrap with logging for debugging
            Ok(Arc::new(LoggingGateway::new(config, inner_gateway)))
        }
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
    inner: Arc<dyn ProviderAdapter>,
}

impl LoggingGateway {
    pub fn new(config: GatewayConfig, inner: Arc<dyn ProviderAdapter>) -> Self {
        Self { config, inner }
    }
}

#[async_trait]
impl ProviderAdapter for LoggingGateway {
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

pub(super) fn should_retry_locally(
    gateway_type: GatewayType,
    policy: &GatewayRetryConfig,
    error: &BackendError,
    attempt: u32,
) -> bool {
    if gateway_type.delegates_retry_to_adapter() {
        // Some gateway retry policies are passed to the provider adapter as request
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
impl ProviderAdapter for BuiltinHttpGateway {
    fn gateway_type(&self) -> GatewayType {
        self.config.backend_type
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
impl ProviderAdapter for MockGateway {
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
