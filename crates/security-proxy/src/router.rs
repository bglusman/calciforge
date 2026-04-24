//! HTTP router construction for the security-proxy service.
//!
//! Extracted from main.rs so integration tests can build the exact same
//! router without spawning the binary. Drift between test and prod is
//! impossible because both call `build_app(state)`.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use tracing::warn;

use crate::proxy::{health_handler, proxy_handler, SecurityProxy};

/// Build the axum router the security-proxy binary serves.
///
/// Routes:
///   - `GET /health` — liveness probe (trivial JSON).
///   - `GET /vault/:secret` — resolve a secret through the shared
///     env → fnox → vaultwarden resolver.
///   - fallback → `proxy_handler` (the MITM forward proxy for every
///     other URL, with outbound/inbound scanning + credential
///     injection).
pub fn build_app(state: Arc<SecurityProxy>) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/vault/:secret", get(vault_handler))
        .fallback(proxy_handler)
        .with_state(state)
}

/// GET /vault/:secret handler. Returns 200 with the resolved token on
/// success, 404 with a bland message on failure. We deliberately do
/// NOT include `e.to_string()` in the error body — resolver errors
/// can name the env vars or vault URLs probed, which leaks shape of
/// the secret store.
pub async fn vault_handler(
    State(_state): State<Arc<SecurityProxy>>,
    Path(secret_name): Path<String>,
) -> impl IntoResponse {
    match onecli_client::vault::get_secret(&secret_name).await {
        Ok(token) => Json(serde_json::json!({
            "status": "ok",
            "secret": secret_name,
            "token": token,
        }))
        .into_response(),
        Err(e) => {
            warn!(secret = %secret_name, error = %e, "Vault lookup failed");
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
