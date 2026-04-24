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
use tracing::debug;

use crate::proxy::{health_handler, proxy_handler, SecurityProxy};

/// Build the axum router the security-proxy binary serves.
///
/// Routes:
///   - `GET /health` — liveness probe (trivial JSON).
///   - `GET /vault/:secret` — resolve a secret via the shared
///     `onecli_client::vault::get_secret` resolver. The backend chain
///     (env, fnox, vaultwarden) is an implementation detail of that
///     library and varies by branch/feature set; callers here don't
///     need to know which layer resolved the value.
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
/// success, 404 with a bland message on failure.
///
/// Neither the client response body nor ops logs contain the resolver's
/// raw error text: a verbose error would name the env vars probed and
/// the vault URL queried, either of which reveals shape of the secret
/// store to anyone reading logs (or 403 bodies). We log the secret
/// *name* at `debug!` so you can correlate requests to attempts during
/// incident investigation, but the underlying error stays redacted.
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
        Err(_) => {
            // Name only; no error text. If you need to debug, enable
            // `RUST_LOG=onecli_client=debug` to see the underlying
            // resolver's own debug output (which is scoped to that lib
            // and can be routed to a non-shared log destination in
            // production).
            debug!(secret = %secret_name, "vault lookup failed");
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
