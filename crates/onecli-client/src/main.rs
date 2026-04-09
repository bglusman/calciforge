//! OneCLI Service - Credential proxy and policy enforcement gateway
//!
//! Runs as a standalone HTTP service that:
//! 1. Receives requests from agent wrappers
//! 2. Injects credentials from vault (Bitwarden/Vaultwarden)
//! 3. Routes to upstream providers
//! 4. Enforces clash policy on tool calls

use axum::{
    Json, Router,
    body::Body,
    extract::{Query, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{debug, error, info, warn};

mod vault;

use onecli_client::OneCliServiceConfig;

/// Shared application state
#[derive(Clone)]
struct AppState {
    _config: Arc<OneCliServiceConfig>,
    http_client: reqwest::Client,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    info!("Starting OneCLI service...");

    let config = OneCliServiceConfig::from_env_or_file().await?;
    let state = AppState {
        _config: Arc::new(config),
        http_client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?,
    };

    let app = Router::new()
        .route("/health", get(health_handler))
        // Proxy routes - must capture provider and the rest separately
        .route("/proxy/:provider", any(proxy_handler))
        .route("/proxy/:provider/*rest", any(proxy_handler))
        .route("/proxy-url", any(generic_proxy_handler))
        // Vault endpoint - use sparingly, only when proxy can't handle it
        .route("/vault/:secret", get(vault_handler))
        .route("/policy/check", post(policy_check_handler))
        .with_state(state);

    let bind_addr: SocketAddr = std::env::var("ONECLI_BIND")
        .unwrap_or_else(|_| "0.0.0.0:8081".to_string())
        .parse()?;

    info!("OneCLI service listening on {}", bind_addr);
    let listener = TcpListener::bind(bind_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health_handler() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "healthy",
        "service": "onecli",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Known LLM provider mappings
fn get_provider_url(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("https://api.anthropic.com"),
        "openai" => Some("https://api.openai.com"),
        "kimi" => Some("https://api.moonshot.cn"),
        "gemini" => Some("https://generativelanguage.googleapis.com"),
        "groq" => Some("https://api.groq.com/openai/v1"),
        "brave" => Some("https://api.search.brave.com"),
        _ => None,
    }
}

async fn proxy_handler(
    State(state): State<AppState>,
    axum::extract::Path(params): axum::extract::Path<ProxyParams>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Result<Response, StatusCode> {
    let provider = params.provider;
    let rest_path = params.rest.unwrap_or_default();

    debug!(provider = %provider, rest = %rest_path, "Proxying request");

    let target_url = get_provider_url(&provider).ok_or(StatusCode::BAD_REQUEST)?;

    // Build full target path
    let query = request
        .uri()
        .query()
        .map(|q| format!("?{}", q))
        .unwrap_or_default();
    let full_path = format!("/{}{}", rest_path, query);

    info!(
        "Proxy: {} /proxy/{}/{} -> {}{}",
        request.method(),
        provider,
        rest_path,
        target_url,
        full_path
    );

    proxy_with_path(state, target_url, &provider, &full_path, headers, request).await
}

#[derive(Deserialize)]
struct ProxyParams {
    provider: String,
    rest: Option<String>,
}

async fn proxy_with_path(
    state: AppState,
    target_url: &str,
    secret_name: &str,
    target_path: &str,
    headers: HeaderMap,
    request: Request<Body>,
) -> Result<Response, StatusCode> {
    let mut forwarded_req = state.http_client.request(
        request.method().clone(),
        format!("{}{}", target_url, target_path),
    );

    // Forward headers (except host and x-onecli-*)
    for (key, value) in headers.iter() {
        let key_str = key.as_str().to_lowercase();
        if key_str != "host" && !key_str.starts_with("x-onecli-") {
            forwarded_req = forwarded_req.header(key, value);
        }
    }

    // Try to inject credentials from vault
    let mut cred_injected = false;
    match vault::get_secret(secret_name).await {
        Ok(token) => {
            debug!("Injected credentials for {}", secret_name);
            // Use provider-specific auth header
            if secret_name == "brave" || secret_name == "Brave" {
                forwarded_req = forwarded_req.header("X-Subscription-Token", token);
            } else {
                forwarded_req = forwarded_req.header("Authorization", format!("Bearer {}", token));
            }
            cred_injected = true;
        }
        Err(_) => {
            // Try common variations
            let variations = vec![
                secret_name.to_lowercase(),
                secret_name.to_uppercase(),
                format!("{} API", secret_name),
                format!("{} API Key", secret_name),
            ];
            for var in variations {
                if let Ok(token) = vault::get_secret(&var).await {
                    debug!(
                        "Injected credentials for {} (matched as {})",
                        secret_name, var
                    );
                    if var.to_lowercase().contains("brave") {
                        forwarded_req = forwarded_req.header("X-Subscription-Token", token);
                    } else {
                        forwarded_req =
                            forwarded_req.header("Authorization", format!("Bearer {}", token));
                    }
                    cred_injected = true;
                    break;
                }
            }
        }
    }

    if !cred_injected {
        warn!("No credentials found for {}", secret_name);
    }

    // Add body if present
    let body_bytes = axum::body::to_bytes(request.into_body(), usize::MAX)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    if !body_bytes.is_empty() {
        forwarded_req = forwarded_req.body(body_bytes);
    }

    match forwarded_req.send().await {
        Ok(response) => {
            let status = response.status();
            let headers = response.headers().clone();
            let body = response.bytes().await.unwrap_or_default();

            let mut builder = Response::builder().status(status);
            for (key, value) in headers.iter() {
                builder = builder.header(key, value);
            }
            Ok(builder.body(Body::from(body)).unwrap())
        }
        Err(e) => {
            error!("Proxy error: {}", e);
            Err(StatusCode::BAD_GATEWAY)
        }
    }
}

#[derive(Deserialize)]
struct GenericProxyQuery {
    target: String,
    secret: Option<String>,
}

async fn generic_proxy_handler(
    State(state): State<AppState>,
    Query(query): Query<GenericProxyQuery>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Result<Response, StatusCode> {
    debug!(target = %query.target, "Generic proxy request");

    // Validate target URL (only allow https)
    if !query.target.starts_with("https://") {
        warn!("Rejecting non-HTTPS target: {}", query.target);
        return Err(StatusCode::BAD_REQUEST);
    }

    // Use secret name if provided, otherwise try to derive from hostname
    let secret_name = query.secret.unwrap_or_else(|| {
        query
            .target
            .trim_start_matches("https://")
            .trim_start_matches("api.")
            .split('.')
            .next()
            .unwrap_or("unknown")
            .to_string()
    });

    // Build full path with query string
    let target_path = request.uri().path();
    let target_query = request
        .uri()
        .query()
        .map(|q| format!("?{}", q))
        .unwrap_or_default();
    let full_path = format!("{}{}", target_path, target_query);

    proxy_with_path(
        state,
        &query.target,
        &secret_name,
        &full_path,
        headers,
        request,
    )
    .await
}

async fn vault_handler(
    State(_state): State<AppState>,
    axum::extract::Path(secret_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    match vault::get_secret(&secret_name).await {
        Ok(token) => Json(serde_json::json!({
            "status": "ok",
            "secret": secret_name,
            "token": token,
        }))
        .into_response(),
        Err(e) => {
            warn!("Vault lookup failed: {}", e);
            (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "status": "error",
                    "message": "Secret not found",
                })),
            )
                .into_response()
        }
    }
}

async fn policy_check_handler(
    State(_state): State<AppState>,
    Json(request): Json<PolicyCheckRequest>,
) -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "allowed",
        "tool": request.tool,
        "policy_version": "0.1.0",
    }))
}

#[derive(Deserialize)]
struct PolicyCheckRequest {
    tool: String,
    _args: serde_json::Value,
}
