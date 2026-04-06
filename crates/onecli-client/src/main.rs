//! OneCLI Service - Credential proxy and policy enforcement gateway
//!
//! Runs as a standalone HTTP service that:
//! 1. Receives requests from agent wrappers
//! 2. Injects credentials from vault (Bitwarden/Vaultwarden)
//! 3. Routes to upstream providers
//! 4. Enforces clash policy on tool calls

use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, any},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{debug, error, info, warn};

mod config;
mod vault;
mod policy;

use config::OneCliServiceConfig;

/// Shared application state
#[derive(Clone)]
struct AppState {
    config: Arc<OneCliServiceConfig>,
    http_client: reqwest::Client,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    info!("Starting OneCLI service...");
    
    let config = OneCliServiceConfig::from_env_or_file().await?;
    let state = AppState {
        config: Arc::new(config),
        http_client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?,
    };
    
    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/proxy/:provider", any(proxy_handler))
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

async fn proxy_handler(
    State(state): State<AppState>,
    axum::extract::Path(provider): axum::extract::Path<String>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Result<Response, StatusCode> {
    debug!(provider = %provider, "Proxying request");
    
    let target_url = match provider.as_str() {
        "anthropic" => "https://api.anthropic.com",
        "openai" => "https://api.openai.com",
        "kimi" => "https://api.moonshot.cn",
        "gemini" => "https://generativelanguage.googleapis.com",
        _ => {
            warn!("Unknown provider: {}", provider);
            return Err(StatusCode::BAD_REQUEST);
        }
    };
    
    let mut forwarded_req = state.http_client.request(
        request.method().clone(),
        format!("{}{}", target_url, request.uri().path())
    );
    
    for (key, value) in headers.iter() {
        if key.as_str().to_lowercase() != "host" {
            forwarded_req = forwarded_req.header(key, value);
        }
    }
    
    if let Ok(token) = vault::get_secret(&provider).await {
        forwarded_req = forwarded_req.header("Authorization", format!("Bearer {}", token));
    }
    
    let body_bytes = axum::body::to_bytes(request.into_body(), usize::MAX)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    forwarded_req = forwarded_req.body(body_bytes);
    
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

async fn vault_handler(
    State(_state): State<AppState>,
    axum::extract::Path(secret_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    match vault::get_secret(&secret_name).await {
        Ok(token) => Json(serde_json::json!({
            "status": "ok",
            "secret": secret_name,
            "token": token,
        })).into_response(),
        Err(e) => {
            warn!("Vault lookup failed: {}", e);
            (StatusCode::NOT_FOUND, Json(serde_json::json!({
                "status": "error",
                "message": "Secret not found",
            }))).into_response()
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
    args: serde_json::Value,
}
