//! HTTP router construction for the security-proxy service.
//!
//! Extracted from main.rs so integration tests can build the exact same
//! router without spawning the binary. Drift between test and prod is
//! impossible because both call `build_app(state)`.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use tracing::debug;

use crate::proxy::{health_handler, proxy_handler, SecurityProxy};

/// Env var holding the bearer token required to call `/vault/:secret`.
/// Unset → the vault route returns 503 (refuses to act as an oracle).
/// This is intentionally separate from any cred-injection token; it
/// guards the resolve-and-return path that has no other authn.
const VAULT_TOKEN_ENV: &str = "SECURITY_PROXY_VAULT_TOKEN";

/// Constant-time byte comparison to keep the bearer-token check from
/// leaking length/prefix information via timing. Std doesn't provide
/// one; we keep it tiny rather than pull a crate.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

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
    headers: HeaderMap,
    Path(secret_name): Path<String>,
) -> impl IntoResponse {
    // Defense in depth: even when the binary binds 127.0.0.1 (see
    // main.rs default), a misconfigured deployment that opens it up to
    // 0.0.0.0 would otherwise expose an unauthenticated secret oracle
    // to anyone on the network. Require a bearer token; if the env var
    // is unset, refuse to serve the route at all rather than silently
    // accepting "no token".
    match std::env::var(VAULT_TOKEN_ENV) {
        Ok(expected) if !expected.is_empty() => {
            let provided = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .unwrap_or("");
            if !constant_time_eq(provided.as_bytes(), expected.as_bytes()) {
                debug!(secret = %secret_name, "vault auth failed");
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({"status": "error", "message": "unauthorized"})),
                )
                    .into_response();
            }
        }
        _ => {
            debug!(
                "vault route called but {} unset; refusing as oracle",
                VAULT_TOKEN_ENV
            );
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"status": "error", "message": "vault route disabled"})),
            )
                .into_response();
        }
    }

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
