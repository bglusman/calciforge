//! Model Gateway
//!
//! OpenAI-compatible HTTP server with multi-provider routing, retries,
//! graceful degradation, Traceloop observability, and alloy support.

use std::net::SocketAddr;

use anyhow::Context as _;
use axum::{
    routing::{get, post},
    Router,
};
use tokio::net::TcpListener;
use tracing::info;

use crate::sync::Arc;

use crate::config::{ExecModelConfig, ProxyConfig};
use crate::providers::alloy::AlloyManager;
use crate::providers::ProviderRegistry;

mod alloy_router;
mod auth;
mod backend;
mod exec_gateway;
mod gateway;
mod handlers;
mod openai;
pub(crate) mod routing;
mod streaming;
mod token_estimator;
mod voice_handlers;

// Helicone AI Gateway router (HTTP-based)
#[cfg(feature = "helicone")]
mod helicone_router;

// Traceloop-inspired router
#[cfg(feature = "traceloop")]
mod traceloop;

pub use openai::ChatCompletionRequest;
pub use routing::ProviderEntry;

/// Proxy server state shared across handlers
#[derive(Clone)]
#[allow(dead_code)]
pub struct ProxyState {
    pub alloy_manager: Arc<AlloyManager>,
    pub provider_registry: Arc<ProviderRegistry>,
    pub config: ProxyConfig,
    /// Default gateway — used when no named provider matches the model.
    pub gateway: Arc<dyn gateway::GatewayBackend>,
    /// Named provider entries, in routing priority order.
    /// Entries from `model_routes` come first, then from `providers.models` patterns.
    pub providers: Vec<ProviderEntry>,
    /// Local model lifecycle manager (present when `[local_models]` is configured).
    pub local_manager: Option<Arc<crate::local_model::LocalModelManager>>,
    /// Voice pipeline config (present when `[proxy.voice]` is configured).
    pub voice: Option<crate::voice::VoiceConfig>,
}

/// Normalize an optional API key. Empty strings never enable auth.
fn normalize_api_key(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Read an API key from a file path, stripping surrounding whitespace.
fn read_key_file(path: &std::path::Path) -> anyhow::Result<Option<String>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading API key file {}", path.display()))?;
    Ok(normalize_api_key(&raw))
}

/// Resolve a provider's effective API key: file takes precedence over inline.
fn resolve_api_key(
    api_key: Option<&str>,
    api_key_file: Option<&std::path::Path>,
) -> anyhow::Result<Option<String>> {
    if let Some(file) = api_key_file {
        return read_key_file(file);
    }
    Ok(api_key.and_then(normalize_api_key))
}

/// Start the model gateway HTTP server
pub async fn start_proxy_server(
    mut config: ProxyConfig,
    exec_models: Vec<ExecModelConfig>,
    alloy_manager: Arc<AlloyManager>,
    provider_registry: Arc<ProviderRegistry>,
    local_manager: Option<Arc<crate::local_model::LocalModelManager>>,
) -> anyhow::Result<()> {
    if !config.enabled {
        info!("Proxy server disabled in config");
        return Ok(());
    }

    let addr: SocketAddr = config
        .bind
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid bind address '{}': {}", config.bind, e))?;

    // Resolve the gateway's client-facing API key before sharing config with
    // handlers. `api_key_file` is preferred so deployments can avoid inline
    // TOML secrets while still enforcing Authorization on chat completions.
    config.api_key = resolve_api_key(config.api_key.as_deref(), config.api_key_file.as_deref())?;

    // Resolve the default backend API key (file takes precedence over inline).
    let default_api_key = resolve_api_key(
        config.backend_api_key.as_deref(),
        config.backend_api_key_file.as_deref(),
    )?;

    // Create backend based on config
    let backend_config = match config.backend_type.as_str() {
        "http" => backend::BackendConfig {
            backend_type: backend::BackendType::Http,
            url: Some(config.backend_url.clone()),
            api_key: default_api_key.clone(),
            timeout_seconds: Some(config.timeout_seconds),
            headers: config.headers.clone(),
            ..Default::default()
        },
        "embedded" => backend::BackendConfig {
            backend_type: backend::BackendType::Embedded,
            headers: config.headers.clone(),
            ..Default::default()
        },
        "library" => backend::BackendConfig {
            backend_type: backend::BackendType::Library,
            headers: config.headers.clone(),
            ..Default::default()
        },
        "helicone" => backend::BackendConfig {
            backend_type: backend::BackendType::Helicone,
            helicone_url: Some(config.backend_url.clone()),
            helicone_api_key: default_api_key.clone(),
            timeout_seconds: Some(config.timeout_seconds),
            headers: config.headers.clone(),
            ..Default::default()
        },
        "traceloop" => backend::BackendConfig {
            backend_type: backend::BackendType::Mock, // Traceloop uses gateway, not backend
            ..Default::default()
        },
        _ => backend::BackendConfig {
            backend_type: backend::BackendType::Mock,
            headers: config.headers.clone(),
            ..Default::default()
        },
    };

    info!(
        backend_type = ?backend_config.backend_type,
        header_count = backend_config.headers.as_ref().map(|h| h.len()).unwrap_or_default(),
        "Creating proxy backend"
    );

    let backend = backend::create_backend(&backend_config)
        .map_err(|e| anyhow::anyhow!("Failed to create backend: {}", e))?;

    // Determine gateway type based on configuration
    let gateway_type = if config.backend_type == "helicone" {
        gateway::GatewayType::Helicone
    } else {
        gateway::GatewayType::Direct
    };

    let gateway_config = gateway::GatewayConfig {
        backend_type: gateway_type,
        base_url: Some(config.backend_url.clone()),
        api_key: Some(default_api_key.unwrap_or_default()),
        timeout_seconds: config.timeout_seconds,
        extra_config: None,
        headers: config.headers.clone(),
        retry_enabled: true,
        max_retries: 3,
        retry_base_delay_ms: 1000,
        retry_max_delay_ms: 10000,
    };

    // Create default gateway
    let gateway = gateway::create_gateway(gateway_config, Some(backend))
        .map_err(|e| anyhow::anyhow!("Failed to create gateway: {}", e))?;

    // Build named provider entries from [[proxy.providers]] and [[proxy.model_routes]].
    let providers = routing::build_provider_entries(&config, &exec_models, config.timeout_seconds)?;
    info!(providers = providers.len(), "Named providers loaded");

    let state = ProxyState {
        alloy_manager,
        provider_registry,
        config: config.clone(),
        gateway,
        providers,
        local_manager,
        voice: config.voice.clone(),
    };

    let app = Router::new()
        .route("/v1/chat/completions", post(handlers::chat_completions))
        .route("/v1/models", get(handlers::list_models))
        .route("/health", get(handlers::health_check))
        .route("/control/local/switch", post(handlers::local_model_switch))
        // Voice passthrough — always registered; returns 501 when not configured.
        .route(
            "/v1/audio/transcriptions",
            post(voice_handlers::audio_transcriptions),
        )
        .route("/v1/audio/speech", post(voice_handlers::audio_speech))
        // Tool manifest — always available; reflects what is actually configured.
        .route("/v1/tools/manifest", get(voice_handlers::tools_manifest))
        .with_state(state);

    info!("Starting model gateway on {}", addr);

    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to bind to {}: {}", addr, e))?;

    axum::serve(listener, app)
        .await
        .map_err(|e| anyhow::anyhow!("Server error: {}", e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::resolve_api_key;

    #[test]
    fn resolve_api_key_ignores_empty_inline_key() {
        assert_eq!(resolve_api_key(Some("  "), None).unwrap(), None);
    }

    #[test]
    fn resolve_api_key_trims_inline_key() {
        assert_eq!(
            resolve_api_key(Some(" test-key\n"), None).unwrap(),
            Some("test-key".to_string())
        );
    }
}
