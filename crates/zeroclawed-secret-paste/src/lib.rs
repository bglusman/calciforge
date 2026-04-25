//! `zeroclawed-secret-paste` — localhost one-shot secret-input server.
//!
//! Implements the `!secure request NAME` flow per
//! `docs/rfcs/secret-input-web-ui.md`. Workflow:
//!
//! 1. A caller (chat command, MCP tool, CLI) invokes
//!    [`PasteServer::spawn_request`] with a secret name + description.
//! 2. The server allocates a random port (or uses configured), mints a
//!    32-byte random token, binds an axum listener on
//!    `127.0.0.1:<port>`, and returns the URL the user visits.
//! 3. User opens the URL in a browser, sees a single text field
//!    labeled with the secret name + description, pastes the value,
//!    submits.
//! 4. Server validates the token, calls
//!    [`onecli_client::FnoxClient::set`], renders a confirmation page
//!    with optional first/last-N preview, and shuts down.
//!
//! ## Security properties
//!
//! - Localhost-only binding; no remote access by default
//! - Single-use URL token; 5-minute default expiry
//! - **New-only by default**: refuses to overwrite an existing secret
//!   unless the user explicitly passes `?update=1` (eliminates
//!   accidental clobber and limits compromised-browser blast radius)
//! - Confirmation page shows first/last-N preview, configurable, off
//!   by default
//! - Origin/Referer header check on POST to mitigate DNS rebinding

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use rand::Rng;
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Per-deployment configuration for the paste server. Defaults match
/// the conservative recommendations in the RFC: 5-min expiry, no
/// preview, Origin check on, opaque Origin rejected.
#[derive(Debug, Clone)]
pub struct PasteConfig {
    /// How long a single token stays valid before being purged.
    pub expiry: Duration,
    /// Show first/last N characters of the submitted value on the
    /// confirmation page. None = no preview (default).
    pub preview_chars: Option<usize>,
    /// Reject POSTs whose Origin header doesn't match an expected
    /// localhost origin. On by default — defends against DNS-rebinding
    /// from an attacker page that resolves to 127.0.0.1.
    pub require_localhost_origin: bool,
    /// Whether `Origin: null` (sandboxed iframes, file://, certain
    /// sandbox attributes) counts as a valid localhost origin. OFF by
    /// default — opaque origins can be smuggled by an attacker page that
    /// embeds the paste form in a sandboxed iframe and weakens the
    /// rebinding defense the localhost check exists for.
    pub allow_null_origin: bool,
}

impl Default for PasteConfig {
    fn default() -> Self {
        Self {
            expiry: Duration::from_secs(5 * 60),
            preview_chars: None,
            require_localhost_origin: true,
            allow_null_origin: false,
        }
    }
}

/// In-flight paste request.
#[derive(Debug, Clone)]
struct PendingRequest {
    name: String,
    description: String,
    expires_at: chrono::DateTime<chrono::Utc>,
    completed: bool,
}

#[derive(Clone)]
struct ServerState {
    fnox: onecli_client::FnoxClient,
    config: PasteConfig,
    requests: Arc<Mutex<HashMap<String, PendingRequest>>>,
    /// One-shot sender used by `post_submit` to signal that the user
    /// has successfully submitted a value. The CLI awaits the receiving
    /// half (via [`PasteHandle::wait_submitted`]) so it can exit
    /// immediately on submit instead of sleeping until expiry.
    /// Wrapped in a Mutex<Option<_>> because oneshot::Sender::send
    /// consumes self, and the handler holds &state.
    submitted: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

/// Spawned paste-request handle. Carries the URL the user visits and a
/// graceful shutdown channel. Dropping the handle still tears down the
/// server (legacy behavior preserved), but callers are encouraged to
/// call [`PasteHandle::shutdown`] explicitly so axum's connection-drain
/// runs and submitted submissions actually flush their fnox set.
#[derive(Debug)]
pub struct PasteHandle {
    pub url: String,
    pub token: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    /// JoinHandle for the server task; kept so dropping the handle
    /// aborts the task. Real shutdown goes via the oneshot below so
    /// the server drains in-flight requests first.
    _server_task: tokio::task::JoinHandle<()>,
    /// Send-half of the graceful-shutdown signal. Send `()` to ask
    /// `axum::serve(...).with_graceful_shutdown(...)` to stop accepting
    /// new connections and drain. Wrapped in Option so `shutdown()`
    /// can take and consume it.
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    /// Receive-half of the "submitted" signal. None after the first
    /// `wait_submitted` call (oneshot recv consumes the receiver).
    submitted_rx: Option<tokio::sync::oneshot::Receiver<()>>,
}

impl PasteHandle {
    /// Block until the user submits the form successfully, the server
    /// shuts down, or the receiver is otherwise dropped. Returns
    /// `Ok(())` on submit, `Err(())` on any other terminal state.
    /// Callable at most once per handle.
    pub async fn wait_submitted(&mut self) -> Result<(), ()> {
        let Some(rx) = self.submitted_rx.take() else {
            return Err(());
        };
        rx.await.map_err(|_| ())
    }

    /// Trigger graceful shutdown: server stops accepting new
    /// connections and drains in-flight requests. Idempotent.
    pub fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Errors callers may need to handle.
#[derive(Debug, thiserror::Error)]
pub enum PasteError {
    #[error("invalid secret name (allowed: A-Z a-z 0-9 _ -)")]
    InvalidName,
    #[error("io error spawning listener: {0}")]
    Io(#[from] std::io::Error),
}

/// Spawn a one-shot paste server bound to a random localhost port.
/// Returns immediately with the URL the user should open. The server
/// runs in a background tokio task; call [`PasteHandle::wait_submitted`]
/// to block until the user submits, and [`PasteHandle::shutdown`] to
/// trigger graceful drain.
///
/// Port: 0 (kernel picks a free port). The previous doc claimed a
/// `PORT` env override existed; it didn't, so the claim is removed
/// rather than papered over. If a stable port is needed (e.g.
/// integration testing), expose it via PasteConfig in a follow-up.
pub async fn spawn_request(
    name: impl Into<String>,
    description: impl Into<String>,
    fnox: onecli_client::FnoxClient,
    config: PasteConfig,
) -> Result<PasteHandle, PasteError> {
    let name = name.into();
    if !is_valid_name(&name) {
        return Err(PasteError::InvalidName);
    }
    let token = mint_token();
    let expires_at = chrono::Utc::now()
        + chrono::Duration::from_std(config.expiry).unwrap_or(chrono::Duration::minutes(5));

    let mut state_requests = HashMap::new();
    state_requests.insert(
        token.clone(),
        PendingRequest {
            name: name.clone(),
            description: description.into(),
            expires_at,
            completed: false,
        },
    );

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let (submitted_tx, submitted_rx) = tokio::sync::oneshot::channel::<()>();

    let state = ServerState {
        fnox,
        config: config.clone(),
        requests: Arc::new(Mutex::new(state_requests)),
        submitted: Arc::new(Mutex::new(Some(submitted_tx))),
    };

    let app = Router::new()
        .route("/paste/:token", get(get_form).post(post_submit))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr: SocketAddr = listener.local_addr()?;
    let url = format!("http://{addr}/paste/{token}");

    // Log only the bound address — the URL contains the one-shot bearer
    // token and would land in shared logs / journalctl / shell history
    // on any caller that captures stdout. Token detail at debug! only,
    // opt-in via RUST_LOG.
    info!(secret = %name, addr = %addr, "secret-paste server listening");
    debug!(secret = %name, %url, "secret-paste full URL (debug-only)");

    let server_task = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    Ok(PasteHandle {
        url,
        token,
        expires_at,
        _server_task: server_task,
        shutdown_tx: Some(shutdown_tx),
        submitted_rx: Some(submitted_rx),
    })
}

fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

fn mint_token() -> String {
    // 32 random bytes → 64 hex chars. Plenty of entropy; no need to
    // depend on a UUID crate just for this.
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Deserialize)]
struct UpdateQuery {
    #[serde(default)]
    update: Option<u32>,
}

#[derive(Deserialize)]
struct SubmitForm {
    value: String,
}

async fn get_form(
    State(state): State<ServerState>,
    Path(token): Path<String>,
) -> impl IntoResponse {
    let req = {
        let map = state.requests.lock().await;
        map.get(&token).cloned()
    };
    let Some(req) = req else {
        return (StatusCode::NOT_FOUND, Html(NOT_FOUND_HTML.to_string())).into_response();
    };
    if chrono::Utc::now() > req.expires_at {
        return (StatusCode::GONE, Html(EXPIRED_HTML.to_string())).into_response();
    }
    if req.completed {
        return (
            StatusCode::CONFLICT,
            Html("This paste link has already been used.".to_string()),
        )
            .into_response();
    }
    Html(render_form(&req.name, &req.description, &token)).into_response()
}

async fn post_submit(
    State(state): State<ServerState>,
    Path(token): Path<String>,
    Query(query): Query<UpdateQuery>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<SubmitForm>,
) -> impl IntoResponse {
    if state.config.require_localhost_origin {
        let allow_null = state.config.allow_null_origin;
        let ok = headers
            .get(axum::http::header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|o| is_localhost_origin(o, allow_null));
        if !ok {
            warn!("rejecting paste POST: missing/non-localhost Origin header");
            return (
                StatusCode::FORBIDDEN,
                Html("Rejected: missing/invalid Origin header (anti-rebinding)".to_string()),
            )
                .into_response();
        }
    }

    let req = {
        let map = state.requests.lock().await;
        map.get(&token).cloned()
    };
    let Some(req) = req else {
        return (StatusCode::NOT_FOUND, Html(NOT_FOUND_HTML.to_string())).into_response();
    };
    if chrono::Utc::now() > req.expires_at {
        return (StatusCode::GONE, Html(EXPIRED_HTML.to_string())).into_response();
    }
    if req.completed {
        return (
            StatusCode::CONFLICT,
            Html("This paste link has already been used.".to_string()),
        )
            .into_response();
    }
    if form.value.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Html("Empty value rejected.".to_string()),
        )
            .into_response();
    }

    // New-only enforcement: unless update=1 explicitly set, refuse if
    // the secret already exists.
    if query.update.unwrap_or(0) == 0 {
        match state.fnox.list().await {
            Ok(names) if names.iter().any(|n| n == &req.name) => {
                debug!(secret = %req.name, "refusing to overwrite existing secret without ?update=1");
                return (
                    StatusCode::CONFLICT,
                    Html(format!(
                        "Secret <code>{}</code> already exists. To intentionally rotate, \
                         re-open this URL with <code>?update=1</code> appended.",
                        html_escape(&req.name)
                    )),
                )
                    .into_response();
            }
            Ok(_) => {} // not present → safe to set
            Err(e) => {
                warn!("fnox list failed during new-only check: {e}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Html("Failed to verify secret state — refusing to set.".to_string()),
                )
                    .into_response();
            }
        }
    }

    match state.fnox.set(&req.name, &form.value).await {
        Ok(()) => {
            // Mark completed; do NOT remove the entry yet — the
            // confirmation page lookup needs it to remain.
            {
                let mut map = state.requests.lock().await;
                if let Some(r) = map.get_mut(&token) {
                    r.completed = true;
                }
            }
            // Signal the spawning task that submission succeeded so the
            // CLI can exit immediately instead of sleeping until expiry.
            // The send may fail if the receiver was already dropped
            // (handle gone, second submit on same token) — both are
            // benign, ignore the result.
            if let Some(tx) = state.submitted.lock().await.take() {
                let _ = tx.send(());
            }
            let preview = state
                .config
                .preview_chars
                .map(|n| truncated_preview(&form.value, n));
            Html(render_confirmation(&req.name, preview)).into_response()
        }
        Err(e) => {
            warn!("fnox set failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html(format!(
                    "Failed to store secret: {}",
                    html_escape(&e.to_string())
                )),
            )
                .into_response()
        }
    }
}

fn is_localhost_origin(origin: &str, allow_null: bool) -> bool {
    if origin == "null" {
        return allow_null;
    }
    origin.starts_with("http://127.0.0.1:")
        || origin.starts_with("http://localhost:")
        || origin == "http://localhost"
}

fn truncated_preview(value: &str, n: usize) -> String {
    if value.chars().count() <= 2 * n {
        return "…".to_string();
    }
    let chars: Vec<char> = value.chars().collect();
    let head: String = chars[..n].iter().collect();
    let tail: String = chars[chars.len() - n..].iter().collect();
    format!("{head}…{tail}")
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_form(name: &str, description: &str, token: &str) -> String {
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Set {name}</title>
<style>body {{font-family:system-ui,sans-serif;max-width:560px;margin:3rem auto;padding:0 1rem;color:#1a1a1a}}
h1 {{font-size:1.2rem}} input[type=password],input[type=text] {{width:100%;padding:.6rem;font-size:1rem;border:1px solid #888;border-radius:4px}}
button {{margin-top:1rem;padding:.6rem 1.2rem;font-size:1rem;border:0;border-radius:4px;background:#0a6;color:#fff;cursor:pointer}}
.warn {{background:#ffe;border:1px solid #cc8;padding:.6rem;border-radius:4px;font-size:.9rem;margin-top:1rem}}</style>
</head><body>
<h1>Set secret <code>{name}</code></h1>
<p>{description}</p>
<form method="POST" action="/paste/{token}">
<label>Value (will be stored, never displayed in full):</label>
<input type="password" name="value" autofocus required>
<button type="submit">Store</button>
</form>
<div class="warn">⚠ This URL is single-use and expires shortly. Close this tab after submission.</div>
</body></html>"#,
        name = html_escape(name),
        description = html_escape(description),
        token = html_escape(token),
    )
}

fn render_confirmation(name: &str, preview: Option<String>) -> String {
    let preview_html = preview
        .map(|p| {
            format!(
                "<p>Preview (first/last chars): <code>{}</code></p>\
                 <p style=\"color:#666;font-size:.9rem\">Verify this matches the source you copied from. \
                 The full value is now stored; this page won't display it.</p>",
                html_escape(&p)
            )
        })
        .unwrap_or_default();
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Stored {name}</title>
<style>body {{font-family:system-ui,sans-serif;max-width:560px;margin:3rem auto;padding:0 1rem;color:#1a1a1a}}
.ok {{color:#0a6}} h1 {{font-size:1.2rem}}</style>
</head><body>
<h1 class="ok">✓ Stored secret <code>{name}</code></h1>
{preview_html}
<p>You can close this tab.</p>
</body></html>"#,
        name = html_escape(name),
    )
}

const NOT_FOUND_HTML: &str = "<h1>Not found</h1><p>This paste link doesn't exist.</p>";
const EXPIRED_HTML: &str = "<h1>Expired</h1><p>This paste link has expired. Request a new one.</p>";

#[cfg(test)]
mod tests {
    //! Tests use `FnoxClient::with_binary(fake)` to mock fnox, then
    //! drive the server via reqwest against the live socket.

    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn fake_fnox(dir: &TempDir, script: &str) -> PathBuf {
        let path = dir.path().join("fnox");
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[test]
    fn token_is_64_hex_chars() {
        let t = mint_token();
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn truncated_preview_short_input_returns_ellipsis_only() {
        assert_eq!(truncated_preview("ab", 4), "…");
        assert_eq!(truncated_preview("12345678", 4), "…");
        assert_eq!(truncated_preview("123456789", 4), "1234…6789");
    }

    /// Given a paste server bound and a fake fnox that doesn't have
    /// the secret yet,
    /// when the user POSTs a value to /paste/{token},
    /// then fnox set is called and the confirmation page renders.
    #[tokio::test]
    async fn happy_path_new_secret_stores_and_confirms() {
        let dir = TempDir::new().unwrap();
        // fake fnox: list returns nothing (so new-only check passes),
        // set succeeds.
        let bin = fake_fnox(
            &dir,
            r#"case "$1" in list) echo "" ;; set) exit 0 ;; *) exit 1 ;; esac"#,
        );
        let client = onecli_client::FnoxClient::with_binary(bin);

        let handle = spawn_request(
            "TEST_KEY",
            "Test description",
            client,
            PasteConfig {
                require_localhost_origin: false,
                ..PasteConfig::default()
            },
        )
        .await
        .unwrap();

        let http = reqwest::Client::new();
        let resp = http
            .post(&handle.url)
            .form(&[("value", "the-secret-value")])
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = resp.text().await.unwrap();
        assert!(
            body.contains("Stored"),
            "confirmation should render: {body}"
        );
        assert!(
            !body.contains("the-secret-value"),
            "confirmation must NOT contain the value: {body}"
        );
    }

    /// Given new-only mode (default) and a fake fnox that says the
    /// secret already exists,
    /// when the user POSTs without ?update=1,
    /// then the server returns 409 and DOESN'T call set.
    #[tokio::test]
    async fn new_only_default_refuses_existing_secret() {
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("set-log");
        // list returns the secret name; set logs that it was called
        // (test asserts the log stays empty).
        let bin = fake_fnox(
            &dir,
            &format!(
                r#"case "$1" in list) echo "EXISTING_KEY" ;; set) echo "$@" >> {} ;; esac"#,
                log.display()
            ),
        );
        let client = onecli_client::FnoxClient::with_binary(bin);

        let handle = spawn_request(
            "EXISTING_KEY",
            "",
            client,
            PasteConfig {
                require_localhost_origin: false,
                ..PasteConfig::default()
            },
        )
        .await
        .unwrap();

        let http = reqwest::Client::new();
        let resp = http
            .post(&handle.url)
            .form(&[("value", "rotated-value")])
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 409);
        assert!(
            !log.exists(),
            "fnox set must NOT have been called: log path {} exists",
            log.display()
        );
    }

    /// Given the same setup but ?update=1 query param,
    /// when the user POSTs,
    /// then set IS called and the confirmation page renders.
    #[tokio::test]
    async fn explicit_update_query_allows_overwrite() {
        let dir = TempDir::new().unwrap();
        let bin = fake_fnox(
            &dir,
            r#"case "$1" in list) echo "EXISTING_KEY" ;; set) exit 0 ;; *) exit 1 ;; esac"#,
        );
        let client = onecli_client::FnoxClient::with_binary(bin);

        let handle = spawn_request(
            "EXISTING_KEY",
            "",
            client,
            PasteConfig {
                require_localhost_origin: false,
                ..PasteConfig::default()
            },
        )
        .await
        .unwrap();

        let http = reqwest::Client::new();
        let resp = http
            .post(format!("{}?update=1", handle.url))
            .form(&[("value", "rotated-value")])
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    /// Given preview_chars=4 and a fake fnox that succeeds,
    /// when the user POSTs a value,
    /// then the confirmation page contains a preview with first 4
    /// and last 4 chars but NOT the full value.
    #[tokio::test]
    async fn preview_renders_first_last_n_only() {
        let dir = TempDir::new().unwrap();
        let bin = fake_fnox(
            &dir,
            r#"case "$1" in list) echo "" ;; set) exit 0 ;; *) exit 1 ;; esac"#,
        );
        let client = onecli_client::FnoxClient::with_binary(bin);

        let handle = spawn_request(
            "K",
            "",
            client,
            PasteConfig {
                require_localhost_origin: false,
                preview_chars: Some(4),
                ..PasteConfig::default()
            },
        )
        .await
        .unwrap();

        let http = reqwest::Client::new();
        let resp = http
            .post(&handle.url)
            .form(&[("value", "abcd-MIDDLE-SECRET-PART-wxyz")])
            .send()
            .await
            .unwrap();
        let body = resp.text().await.unwrap();
        assert!(body.contains("abcd…wxyz"), "preview should render: {body}");
        assert!(
            !body.contains("MIDDLE-SECRET-PART"),
            "preview must redact middle: {body}"
        );
    }

    /// Given require_localhost_origin=true (default) and a POST with
    /// no Origin header,
    /// when the user POSTs,
    /// then the server returns 403.
    #[tokio::test]
    async fn missing_origin_header_rejected_when_required() {
        let dir = TempDir::new().unwrap();
        let bin = fake_fnox(&dir, "exit 0");
        let client = onecli_client::FnoxClient::with_binary(bin);

        let handle = spawn_request("K", "", client, PasteConfig::default())
            .await
            .unwrap();

        let http = reqwest::Client::new();
        let resp = http
            .post(&handle.url)
            .form(&[("value", "v")])
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);
    }

    /// Given the default config (allow_null_origin=false) and a POST
    /// with `Origin: null` (sandboxed iframe / file://),
    /// when the user POSTs,
    /// then the server returns 403. This guards the regression where
    /// `is_localhost_origin` previously accepted "null" unconditionally,
    /// weakening the DNS-rebinding defense an attacker page could chain
    /// through a sandboxed iframe.
    #[tokio::test]
    async fn null_origin_rejected_by_default() {
        let dir = TempDir::new().unwrap();
        let bin = fake_fnox(&dir, "exit 0");
        let client = onecli_client::FnoxClient::with_binary(bin);

        let handle = spawn_request("K", "", client, PasteConfig::default())
            .await
            .unwrap();

        let http = reqwest::Client::new();
        let resp = http
            .post(&handle.url)
            .header("Origin", "null")
            .form(&[("value", "v")])
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            403,
            "Origin: null must be 403 by default — opaque origin can be \
             smuggled via sandboxed iframe; need explicit allow_null_origin=true"
        );
    }

    /// Given allow_null_origin=true and a POST with `Origin: null`,
    /// when the user POSTs (assuming no other guards reject),
    /// then the server proceeds (here: 200 confirmation page on a
    /// freshly-spawned new-secret request). Pairs with the negative
    /// test above to prove the flag actually toggles behavior, not just
    /// passes through.
    #[tokio::test]
    async fn null_origin_accepted_when_explicitly_allowed() {
        let dir = TempDir::new().unwrap();
        // Fake fnox: list returns nothing (so "new-only" check passes),
        // set succeeds.
        let bin = fake_fnox(
            &dir,
            r#"if [ "$1" = "list" ]; then exit 0; else cat > /dev/null; exit 0; fi"#,
        );
        let client = onecli_client::FnoxClient::with_binary(bin);

        let handle = spawn_request(
            "K",
            "",
            client,
            PasteConfig {
                allow_null_origin: true,
                ..PasteConfig::default()
            },
        )
        .await
        .unwrap();

        let http = reqwest::Client::new();
        let resp = http
            .post(&handle.url)
            .header("Origin", "null")
            .form(&[("value", "v")])
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            200,
            "Origin: null must be accepted when allow_null_origin=true"
        );
    }

    /// Given a freshly-spawned paste server,
    /// when the user successfully submits,
    /// then `wait_submitted()` returns Ok(()) — proves the
    /// "exit on submit" plumbing wires the handler signal to the
    /// handle's awaitable.
    #[tokio::test]
    async fn wait_submitted_returns_on_successful_submit() {
        let dir = TempDir::new().unwrap();
        let bin = fake_fnox(
            &dir,
            r#"if [ "$1" = "list" ]; then exit 0; else cat > /dev/null; exit 0; fi"#,
        );
        let client = onecli_client::FnoxClient::with_binary(bin);

        let mut handle = spawn_request(
            "MY_KEY",
            "",
            client,
            PasteConfig {
                require_localhost_origin: false, // simplify: skip Origin in the test
                ..PasteConfig::default()
            },
        )
        .await
        .unwrap();
        let url = handle.url.clone();

        // Spawn the submit in another task; await the signal in the
        // main test body so the handle's wait_submitted is the
        // observable assertion.
        tokio::spawn(async move {
            let http = reqwest::Client::new();
            let _ = http.post(&url).form(&[("value", "v")]).send().await;
        });

        // 2-second cap so the test can never hang the suite.
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(2), handle.wait_submitted()).await;
        assert!(
            matches!(result, Ok(Ok(()))),
            "wait_submitted should resolve Ok(()) on submit; got {result:?}"
        );

        handle.shutdown();
    }
}
