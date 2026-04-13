//! Alloy Model Proxy Server
//!
//! OpenAI-compatible HTTP server that routes requests through Alloy-managed
//! model selection with graceful degradation and analytics.

use std::net::SocketAddr;

use axum::{
    routing::{get, post},
    Router,
};
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::sync::Arc;

use crate::providers::alloy::AlloyManager;
use crate::providers::ProviderRegistry;
use crate::config::ProxyConfig;

mod alloy_router;
mod auth;
mod backend;
mod gateway;
mod handlers;
mod openai;
mod streaming;

// Helicone AI Gateway router (HTTP-based)
#[cfg(feature = "helicone")]
mod helicone_router;

// Traceloop-inspired router
#[cfg(feature = "traceloop")]
mod traceloop;

pub use openai::ChatCompletionRequest;

/// Proxy server state shared across handlers
#[derive(Clone)]
#[allow(dead_code)]
pub struct ProxyState {
    pub alloy_manager: Arc<AlloyManager>,
    pub provider_registry: Arc<ProviderRegistry>,
    pub config: ProxyConfig,
    pub backend: Arc<dyn backend::OneCliBackend>,
    pub gateway: Option<Arc<dyn gateway::GatewayBackend>>,
    pub alloy_router: Option<Arc<alloy_router::AlloyRouter>>,
    
    #[cfg(feature = "traceloop")]
    pub traceloop_router: Option<Arc<traceloop::TraceloopRouter>>,
    
    #[cfg(feature = "helicone")]
    pub helicone_router: Option<Arc<helicone_router::HeliconeRouter>>,
}

/// Start the Alloy proxy HTTP server
pub async fn start_proxy_server(
    config: ProxyConfig,
    alloy_manager: Arc<AlloyManager>,
    provider_registry: Arc<ProviderRegistry>,
) -> anyhow::Result<()> {
    if !config.enabled {
        info!("Proxy server disabled in config");
        return Ok(());
    }

    let addr: SocketAddr = config.bind.parse()
        .map_err(|e| anyhow::anyhow!("Invalid bind address '{}': {}", config.bind, e))?;

    // Create backend based on config
    let backend_config = match config.backend_type.as_str() {
        "http" => {
            let api_key = config.backend_api_key.clone()
                .unwrap_or_else(|| "Cryptonomicon381!".to_string());
            backend::BackendConfig {
                backend_type: backend::BackendType::Http,
                url: Some(config.backend_url.clone()),
                api_key: Some(api_key),
                timeout_seconds: Some(config.timeout_seconds),
                ..Default::default()
            }
        }
        "embedded" => backend::BackendConfig {
            backend_type: backend::BackendType::Embedded,
            ..Default::default()
        },
        "library" => backend::BackendConfig {
            backend_type: backend::BackendType::Library,
            ..Default::default()
        },
        "helicone" => backend::BackendConfig {
            backend_type: backend::BackendType::Helicone,
            helicone_url: Some(config.backend_url.clone()),
            helicone_api_key: config.backend_api_key.clone(),
            timeout_seconds: Some(config.timeout_seconds),
            ..Default::default()
        },
        _ => backend::BackendConfig {
            backend_type: backend::BackendType::Mock,
            ..Default::default()
        },
    };
    
    info!(backend_type = ?backend_config.backend_type, "Creating proxy backend");
    
    let backend = backend::create_backend(&backend_config)
        .map_err(|e| anyhow::anyhow!("Failed to create backend: {}", e))?;

    // Create Helicone router (replacing Traceloop) - only if helicone feature is enabled
    #[cfg(feature = "helicone")]
    let helicone_router = {
        // Try to get Helicone configuration
        let helicone_api_key = config.backend_api_key.clone();
        
        // Create Helicone router configuration
        let helicone_config = helicone_router::HeliconeRouterConfig {
            base_url: config.backend_url.clone(),
            api_key: helicone_api_key.unwrap_or_default(),
            timeout_seconds: config.timeout_seconds,
            router_name: "ai".to_string(),
            enable_caching: true,
            cache_ttl_seconds: 300,
        };
        
        match helicone_router::HeliconeRouter::new(helicone_config.clone()) {
            Ok(router) => {
                info!("Created HeliconeRouter with gateway at {}", helicone_config.base_url);
                Some(Arc::new(router))
            }
            Err(e) => {
                warn!(error = %e, "Failed to create HeliconeRouter, falling back to legacy backend");
                None
            }
        }
    };
    
    // Keep Traceloop router for backward compatibility (disabled)
    #[cfg(feature = "traceloop")]
    let traceloop_router = None;
    
    // Keep old AlloyRouter for backward compatibility (but don't create it)
    let alloy_router = None;

    let state = ProxyState {
        alloy_manager,
        provider_registry,
        config: config.clone(),
        backend,
        gateway: None, // TODO: Create gateway based on config
        alloy_router,
        #[cfg(feature = "traceloop")]
        traceloop_router,
        #[cfg(feature = "helicone")]
        helicone_router,
    };

    let app = Router::new()
        .route("/v1/chat/completions", post(handlers::chat_completions))
        .route("/v1/models", get(handlers::list_models))
        .route("/health", get(handlers::health_check))
        .with_state(state);

    info!("Starting Alloy proxy server on {}", addr);
    
    let listener = TcpListener::bind(&addr).await
        .map_err(|e| anyhow::anyhow!("Failed to bind to {}: {}", addr, e))?;

    axum::serve(listener, app)
        .await
        .map_err(|e| anyhow::anyhow!("Server error: {}", e))?;

    Ok(())
}


