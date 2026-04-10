use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use reqwest::Client;
use tracing::info;
use tracing_subscriber;

use security_gateway::audit::AuditLogger;
use security_gateway::config::GatewayConfig;
use security_gateway::credentials::CredentialInjector;
use security_gateway::proxy::{health_handler, proxy_handler, ProxyState};
use security_gateway::scanner::{ExfilScanner, InjectionScanner};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "security_gateway=info".into()),
        )
        .init();

    let config = GatewayConfig::default();
    let mut credentials = CredentialInjector::new();
    credentials.load_from_env();

    let state = Arc::new(ProxyState {
        config: config.clone(),
        exfil_scanner: ExfilScanner::new(),
        injection_scanner: InjectionScanner::new(),
        credentials,
        audit: AuditLogger::new(),
        http_client: Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?,
    });

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
