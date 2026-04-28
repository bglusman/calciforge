use std::sync::Arc;

use tracing::{error, info};

use adversary_detector::{RateLimitConfig, ScannerCheckConfig, ScannerConfig};
use security_proxy::agent_config::AgentsConfig;
use security_proxy::config::GatewayConfig;
use security_proxy::proxy::SecurityProxy;
use security_proxy::router::build_app;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "security_proxy=info".into()),
        )
        .init();

    let port = std::env::var("SECURITY_PROXY_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| GatewayConfig::default().port);

    let mut config = GatewayConfig {
        port,
        ..GatewayConfig::default()
    };
    if let Ok(url) = std::env::var("SECURITY_PROXY_REMOTE_SCANNER_URL") {
        if !url.trim().is_empty() {
            let fail_closed = std::env::var("SECURITY_PROXY_REMOTE_SCANNER_FAIL_CLOSED")
                .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
                .unwrap_or(false);
            if config.scanner_checks.is_empty() {
                config.scanner_checks = ScannerConfig::default_checks();
            }
            let already_configured = config.scanner_checks.iter().any(|check| {
                matches!(
                    check,
                    ScannerCheckConfig::RemoteHttp {
                        url: configured,
                        ..
                    } if configured == &url
                )
            });
            if !already_configured {
                config
                    .scanner_checks
                    .push(ScannerCheckConfig::RemoteHttp { url, fail_closed });
            }
        }
    }
    let scanner_config = ScannerConfig {
        checks: config.scanner_checks.clone(),
        ..ScannerConfig::default()
    };

    // Build unified security proxy
    let mut proxy =
        SecurityProxy::new(config.clone(), scanner_config, RateLimitConfig::default()).await;

    // Load credentials from ZEROGATE_KEY_* env vars (legacy)
    proxy.credentials.load_from_env();

    // Load from agents.json config
    let agents_config_path =
        std::env::var("AGENT_CONFIG").unwrap_or_else(|_| "/etc/calciforge/agents.json".into());

    if let Ok(agents_config) = AgentsConfig::load(&agents_config_path) {
        info!(
            "Loaded {} agent(s) from {}",
            agents_config.agents.len(),
            agents_config_path
        );

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

    // The router is built by the library so tests can spin up the same
    // routes in-process without spawning the binary. See
    // `security_proxy::router::build_app`.
    let app = build_app(state);

    // Default to loopback-only because the router exposes a
    // `GET /vault/:secret` endpoint that resolves to a real token. Even
    // with the bearer-token guard we added, having the binary bind
    // 0.0.0.0 by default put the entire LAN one network hop from a
    // secret-exfil attempt. Operators who need remote access set
    // SECURITY_PROXY_BIND=0.0.0.0 explicitly (and should pair it with
    // SECURITY_PROXY_VAULT_TOKEN, which gates the vault route).
    let bind_host = std::env::var("SECURITY_PROXY_BIND").unwrap_or_else(|_| "127.0.0.1".into());
    let addr = format!("{}:{}", bind_host, port);
    info!("Security proxy listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
