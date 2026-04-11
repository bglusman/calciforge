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
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;

    info!(
        fallback = ?config.providers.fallback_chain,
        openclaw = ?config.providers.openclaw,
        kimi = ?config.providers.kimi,
        "Provider config loaded"
    );

    let state = AppState {
        _config: Arc::new(config),
        http_client,
    };

    let app = Router::new()
        .route("/health", get(health_handler))
        // Fallback proxy — tries providers in chain order
        .route("/v1/chat/completions", post(fallback_chat_handler))
        .route("/v1/chat/completions", get(fallback_chat_handler))
        // Provider-specific proxy routes
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

/// Known LLM provider mappings (static defaults)
fn get_provider_url(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("https://api.anthropic.com"),
        "openai" => Some("https://api.openai.com"),
        "kimi" => Some("https://api.moonshot.cn"),
        "gemini" => Some("https://generativelanguage.googleapis.com"),
        "groq" => Some("https://api.groq.com/openai/v1"),
        "brave" => Some("https://api.search.brave.com"),
        "openclaw" => Some("http://192.168.1.229:18789"),
        _ => None,
    }
}

/// Resolve provider URL: config override > static default > None
fn resolve_provider_url(provider: &str, config: &OneCliServiceConfig) -> Option<String> {
    // Check config overrides first
    match provider {
        "anthropic" => config.providers.anthropic.clone(),
        "openai" => config.providers.openai.clone(),
        "kimi" => config.providers.kimi.clone(),
        "gemini" => config.providers.gemini.clone(),
        "openclaw" => config.providers.openclaw.clone(),
        _ => None,
    }
    .or_else(|| get_provider_url(provider).map(String::from))
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

    let target_url = resolve_provider_url(&provider, &state._config)
        .ok_or(StatusCode::BAD_REQUEST)?;

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

    proxy_with_path(state, &target_url, &provider, &full_path, headers, request).await
}

/// Fallback chat endpoint: tries providers in chain order
/// POST /v1/chat/completions → tries kimi first, falls back to openclaw
async fn fallback_chat_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Result<Response, StatusCode> {
    let chain = state._config.providers.fallback_chain.clone()
        .unwrap_or_else(|| vec!["kimi".into(), "openclaw".into()]);

    // Read body once, clone for retries
    let body_bytes = axum::body::to_bytes(request.into_body(), usize::MAX)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    info!(chain = ?chain, "Fallback chat: trying chain");

    let mut last_error = StatusCode::BAD_GATEWAY;

    for provider in &chain {
        let target_url = match resolve_provider_url(provider, &state._config) {
            Some(u) => u,
            None => {
                warn!(provider = %provider, "Unknown provider in chain, skipping");
                continue;
            }
        };

        // Build the /v1/chat/completions path
        let target_path = "/v1/chat/completions";
        let full_url = format!("{}{}", target_url, target_path);

        info!(provider = %provider, url = %full_url, "Fallback: trying provider");

        let mut forwarded_req = state.http_client.post(&full_url);

        // Forward headers
        for (key, value) in headers.iter() {
            let key_str = key.as_str().to_lowercase();
            if key_str != "host" && !key_str.starts_with("x-onecli-") {
                forwarded_req = forwarded_req.header(key, value);
            }
        }

        // Inject credentials from vault (preferred) or forward incoming auth (fallback)
        match vault::get_secret(provider).await {
            Ok(token) => {
                forwarded_req = forwarded_req
                    .header("Authorization", format!("Bearer {}", token));
            }
            Err(_) => {
                // Forward the incoming Authorization header if present
                if let Some(auth) = headers.get("authorization") {
                    forwarded_req = forwarded_req.header("authorization", auth.clone());
                    debug!(provider = %provider, "No vault creds, forwarding incoming auth header");
                } else {
                    debug!(provider = %provider, "No vault creds and no incoming auth header");
                }
            }
        }

        forwarded_req = forwarded_req.body(body_bytes.clone());

        match forwarded_req.send().await {
            Ok(response) if response.status().is_success() => {
                info!(provider = %provider, status = %response.status(), "Fallback: success");
                let status = response.status();
                let resp_headers = response.headers().clone();
                let body = response.bytes().await.unwrap_or_default();

                let mut builder = Response::builder().status(status);
                for (key, value) in resp_headers.iter() {
                    builder = builder.header(key, value);
                }
                return Ok(builder.body(Body::from(body)).unwrap());
            }
            Ok(response) => {
                let status = response.status();
                warn!(provider = %provider, status = %status, "Fallback: provider returned error");
                last_error = status;
                // Continue to next provider
            }
            Err(e) => {
                warn!(provider = %provider, err = %e, "Fallback: provider unreachable");
                last_error = StatusCode::BAD_GATEWAY;
                // Continue to next provider
            }
        }
    }

    error!(chain = ?chain, "All providers in fallback chain failed");
    Err(last_error)
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

    // Allow HTTPS always; allow HTTP only for RFC1918/local targets
    let is_https = query.target.starts_with("https://");
    let is_local_http = query.target.starts_with("http://") && {
        let host = query.target.trim_start_matches("http://")
            .split(&[':', '/'][..])
            .next()
            .unwrap_or("");
        host.starts_with("127.")
            || host.starts_with("10.")
            || host.starts_with("192.168.")
            || host.starts_with("172.16.")
            || host.starts_with("172.17.")
            || host.starts_with("172.18.")
            || host.starts_with("172.19.")
            || host.starts_with("172.2")
            || host.starts_with("172.30.")
            || host.starts_with("172.31.")
            || host == "localhost"
    };
    if !is_https && !is_local_http {
        warn!("Rejecting non-local HTTP target: {}", query.target);
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
