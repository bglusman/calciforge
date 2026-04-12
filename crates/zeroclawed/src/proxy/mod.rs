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
mod handlers;
mod openai;
mod streaming;

pub use openai::ChatCompletionRequest;

/// Proxy server state shared across handlers
#[derive(Clone)]
#[allow(dead_code)]
pub struct ProxyState {
    pub alloy_manager: Arc<AlloyManager>,
    pub provider_registry: Arc<ProviderRegistry>,
    pub config: ProxyConfig,
    pub backend: Arc<dyn backend::OneCliBackend>,
    pub alloy_router: Option<Arc<alloy_router::AlloyRouter>>,
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
        _ => backend::BackendConfig {
            backend_type: backend::BackendType::Mock,
            ..Default::default()
        },
    };
    
    info!(backend_type = ?backend_config.backend_type, "Creating proxy backend");
    
    let backend = backend::create_backend(&backend_config)
        .map_err(|e| anyhow::anyhow!("Failed to create backend: {}", e))?;

    // Create AlloyRouter if we have API keys
    let alloy_router = {
        // Try to get API keys from config or environment
        let deepseek_api_key = config.backend_api_key.clone();
        
        // Try to get Kimi API key from environment
        let kimi_api_key = std::env::var("KIMI_API_KEY").ok();
        
        // Only create AlloyRouter if we have at least one API key
        if deepseek_api_key.is_some() || kimi_api_key.is_some() {
            match alloy_router::AlloyRouter::default_with_backends(
                deepseek_api_key,
                kimi_api_key,
            ) {
                Ok(router) => {
                    info!("Created AlloyRouter with {} provider(s)", 
                        router.providers_count());
                    Some(Arc::new(router))
                }
                Err(e) => {
                    warn!(error = %e, "Failed to create AlloyRouter, falling back to legacy backend");
                    None
                }
            }
        } else {
            info!("No API keys available for AlloyRouter, using legacy backend only");
            None
        }
    };

    let state = ProxyState {
        alloy_manager,
        provider_registry,
        config: config.clone(),
        backend,
        alloy_router,
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


