//! Model access boundary
//!
//! OpenAI-compatible HTTP server with multi-provider routing, retries,
//! graceful degradation, optional external provider adapters, and synthetic
//! model routing.

use std::net::SocketAddr;

use anyhow::Context as _;
use axum::{
    Router,
    routing::{get, post},
};
use tokio::net::TcpListener;
use tracing::info;

use crate::sync::Arc;

use crate::config::{ModelShortcutConfig, ProxyConfig};
use crate::providers::ProviderRegistry;
use crate::providers::alloy::AlloyManager;

mod auth;
mod backend;
mod gateway;
mod handlers;
pub(crate) mod model_resolver;
mod openai;
pub(crate) mod routing;
mod streaming;
mod token_estimator;
mod voice_handlers;

// Helicone AI Gateway router (HTTP-based)
#[cfg(feature = "helicone")]
mod helicone_router;

pub use openai::ChatCompletionRequest;
pub use routing::ProviderEntry;

/// Proxy server state shared across handlers
#[derive(Clone)]
#[allow(dead_code)]
pub struct ProxyState {
    pub alloy_manager: Arc<AlloyManager>,
    pub provider_registry: Arc<ProviderRegistry>,
    pub config: ProxyConfig,
    /// Root-level `[[model_shortcuts]]` aliases available to direct proxy requests.
    pub model_shortcuts: Vec<ModelShortcutConfig>,
    /// Legacy root provider adapter — used only when no named provider matches the model.
    pub gateway: Arc<dyn gateway::ProviderAdapter>,
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

const SUPPORTED_ROOT_GATEWAY_BACKEND_TYPES: &[&str] = &["http", "helicone", "mock"];

pub(crate) fn supported_root_gateway_backend_types() -> &'static [&'static str] {
    SUPPORTED_ROOT_GATEWAY_BACKEND_TYPES
}

fn gateway_type_for_backend_type(backend_type: &str) -> gateway::GatewayType {
    match backend_type {
        "helicone" => gateway::GatewayType::Helicone,
        "mock" => gateway::GatewayType::Mock,
        _ => gateway::GatewayType::BuiltinHttp,
    }
}

/// Return true when the configured root gateway is authoritative for model IDs
/// that are not enumerated in Calciforge provider routes.
pub(crate) fn backend_accepts_unlisted_models(backend_type: &str) -> bool {
    matches!(backend_type, "http" | "helicone")
}

fn validate_explicit_provider_selection(config: &ProxyConfig) -> anyhow::Result<()> {
    if config.providers.is_empty() && config.backend_type == "mock" {
        anyhow::bail!(
            "proxy.enabled=true requires at least one explicit [[proxy.providers]] adapter or an explicit non-mock root backend_type. The mock adapter is test-only and is not a production default."
        );
    }
    if matches!(config.backend_type.as_str(), "http" | "helicone")
        && config.backend_url.trim().is_empty()
    {
        anyhow::bail!(
            "proxy.enabled=true with root backend_type='{}' requires backend_url. Use backend_type='mock' for explicit-provider-only configs where unmatched models should fail instead of falling back to a root provider.",
            config.backend_type
        );
    }
    Ok(())
}

/// Resolve all per-agent proxy API key files into in-memory keys before the
/// config is shared with request handlers.
fn resolve_proxy_agent_api_keys(config: &mut ProxyConfig) -> anyhow::Result<()> {
    for agent in &mut config.agents {
        if let Some(file) = agent.api_key_file.as_deref() {
            agent.api_key = read_key_file(file)
                .with_context(|| format!("reading API key file for proxy agent '{}'", agent.id))?;
        } else {
            agent.api_key = agent.api_key.as_deref().and_then(normalize_api_key);
        }
    }
    Ok(())
}

/// Start the model gateway HTTP server
pub async fn start_proxy_server(
    mut config: ProxyConfig,
    model_shortcuts: Vec<ModelShortcutConfig>,
    alloy_manager: Arc<AlloyManager>,
    provider_registry: Arc<ProviderRegistry>,
    local_manager: Option<Arc<crate::local_model::LocalModelManager>>,
) -> anyhow::Result<()> {
    if !config.enabled {
        info!("Proxy server disabled in config");
        return Ok(());
    }
    validate_explicit_provider_selection(&config)?;

    let addr: SocketAddr = config
        .bind
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid bind address '{}': {}", config.bind, e))?;

    // Resolve the gateway's client-facing API keys before sharing config with
    // handlers. `*_key_file` is preferred so deployments can avoid inline TOML
    // secrets while still enforcing Authorization.
    config.api_key = resolve_api_key(config.api_key.as_deref(), config.api_key_file.as_deref())?;
    config.secret_control_api_key = resolve_api_key(
        config.secret_control_api_key.as_deref(),
        config.secret_control_api_key_file.as_deref(),
    )?;
    resolve_proxy_agent_api_keys(&mut config)?;

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
        "helicone" => backend::BackendConfig {
            backend_type: backend::BackendType::Helicone,
            helicone_url: Some(config.backend_url.clone()),
            helicone_api_key: default_api_key.clone(),
            timeout_seconds: Some(config.timeout_seconds),
            headers: config.headers.clone(),
            ..Default::default()
        },
        "mock" => backend::BackendConfig {
            backend_type: backend::BackendType::Mock,
            headers: config.headers.clone(),
            ..Default::default()
        },
        other => anyhow::bail!(
            "Unsupported proxy backend_type '{}'. Supported root provider adapters: {}",
            other,
            supported_root_gateway_backend_types().join(", ")
        ),
    };

    info!(
        backend_type = ?backend_config.backend_type,
        header_count = backend_config.headers.as_ref().map(|h| h.len()).unwrap_or_default(),
        "Creating proxy backend"
    );

    let backend = backend::create_backend(&backend_config)
        .map_err(|e| anyhow::anyhow!("Failed to create backend: {}", e))?;

    // Determine gateway type based on configuration
    let gateway_type = gateway_type_for_backend_type(&config.backend_type);

    let gateway_config = gateway::GatewayConfig {
        backend_type: gateway_type,
        base_url: Some(config.backend_url.clone()),
        api_key: Some(default_api_key.unwrap_or_default()),
        timeout_seconds: config.timeout_seconds,
        extra_config: None,
        headers: config.headers.clone(),
        retry: config.retry.clone(),
        ui_url: config.gateway_ui_url.clone(),
    };

    // Create default provider adapter
    let gateway = gateway::create_gateway(gateway_config, Some(backend))
        .map_err(|e| anyhow::anyhow!("Failed to create gateway: {}", e))?;

    // Build named provider entries from [[proxy.providers]] and [[proxy.model_routes]].
    let providers = routing::build_provider_entries(&config, config.timeout_seconds)?;
    info!(providers = providers.len(), "Named providers loaded");

    let state = ProxyState {
        alloy_manager,
        provider_registry,
        config: config.clone(),
        model_shortcuts,
        gateway,
        providers,
        local_manager,
        voice: config.voice.clone(),
    };

    let app = Router::new()
        .route("/v1/chat/completions", post(handlers::chat_completions))
        .route("/v1/models", get(handlers::list_models))
        .route("/health", get(handlers::health_check))
        .route("/gateway", get(handlers::gateway_info))
        .route("/gateway/ui", get(handlers::gateway_ui_redirect))
        .route("/control/local/switch", post(handlers::local_model_switch))
        .route("/control/secrets/list", get(handlers::secret_list))
        .route(
            "/control/secrets/ref/:name",
            get(handlers::secret_reference),
        )
        .route("/control/secrets/set", post(handlers::secret_set))
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
    use super::{
        backend_accepts_unlisted_models, gateway, gateway_type_for_backend_type, resolve_api_key,
        supported_root_gateway_backend_types,
    };

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

    #[test]
    fn gateway_type_for_backend_type_preserves_supported_external_engines() {
        assert_eq!(
            gateway_type_for_backend_type("helicone"),
            gateway::GatewayType::Helicone
        );
        assert_eq!(
            gateway_type_for_backend_type("http"),
            gateway::GatewayType::BuiltinHttp
        );
        assert_eq!(
            gateway_type_for_backend_type("mock"),
            gateway::GatewayType::Mock
        );
    }

    #[test]
    fn unlisted_model_acceptance_is_shared_for_runtime_and_doctor() {
        assert!(backend_accepts_unlisted_models("helicone"));
        assert!(backend_accepts_unlisted_models("http"));
        assert!(!backend_accepts_unlisted_models("mock"));
    }

    #[test]
    fn supported_root_backend_allowlist_is_small_and_explicit() {
        assert_eq!(
            supported_root_gateway_backend_types(),
            ["http", "helicone", "mock"]
        );
    }
}
