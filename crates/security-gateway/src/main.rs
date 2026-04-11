use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use tracing::{error, info};

use adversary_detector::{RateLimitConfig, ScannerConfig};
use security_gateway::agent_config::AgentsConfig;
use security_gateway::config::GatewayConfig;
use security_gateway::proxy::{health_handler, proxy_handler, SecurityProxy};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "security_gateway=info".into()),
        )
        .init();

    let config = GatewayConfig::default();

    // Build unified security proxy
    let mut proxy = SecurityProxy::new(
        config.clone(),
        ScannerConfig::default(),
        RateLimitConfig::default(),
    )
    .await;

    // Load credentials from ZEROGATE_KEY_* env vars (legacy)
    proxy.credentials.load_from_env();

    // Load from agents.json config
    let agents_config_path =
        std::env::var("AGENT_CONFIG").unwrap_or_else(|_| "/etc/zeroclawed/agents.json".into());

    if let Ok(agents_config) = AgentsConfig::load(&agents_config_path) {
        info!(
            "Loaded {} agent(s) from {}",
            agents_config.agents.len(),
            agents_config_path
        );

        // Auto-load credentials from agent provider configs
        for provider in agents_config.all_providers() {
            if let Ok(api_key) = std::env::var(&provider.env_key) {
                proxy.credentials.add(&provider.name, &api_key);
                info!(
                    "Loaded credential for {} from ${}",
                    provider.name, provider.env_key
                );
            } else {
                info!(
                    "No credential found for {} (${} not set)",
                    provider.name, provider.env_key
                );
            }
        }
    } else {
        error!(
            "Could not load agents config from {}, using env vars only",
            agents_config_path
        );
    }

    let state = Arc::new(proxy);
    let port = config.port;

    let app = Router::new()
        .route("/health", get(health_handler))
        .fallback(proxy_handler)
        .with_state(state);

    let addr = format!("0.0.0.0:{}", port);
    info!("Security Gateway listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
