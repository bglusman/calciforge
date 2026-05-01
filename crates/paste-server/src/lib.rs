//! `paste-server` — one-shot secret-input server.
//!
//! Implements the `!secure request NAME` flow per
//! `docs/rfcs/secret-input-web-ui.md`. Workflow:
//!
//! 1. A caller (chat command, MCP tool, CLI) invokes
//!    [`PasteServer::spawn_request`] with a secret name + description.
//! 2. The server allocates a random port (or uses configured), mints a
//!    32-byte random token, binds an axum listener on the configured
//!    interface, and returns the URL the user visits.
//! 3. User opens the URL in a browser, sees a single text field
//!    labeled with the secret name + description, pastes the value,
//!    submits.
//! 4. Server validates the token, calls
//!    [`secrets_client::FnoxClient::set`], renders a confirmation page
//!    with optional first/last-N preview, and shuts down.
//!
//! ## Security properties
//!
//! - Localhost-only binding for direct CLI use unless configured otherwise
//! - Single-use URL token; 5-minute default expiry
//! - **New-only by default**: refuses to overwrite an existing secret
//!   unless the user explicitly passes `?update=1` (eliminates
//!   accidental clobber and limits compromised-browser blast radius)
//! - Confirmation page shows first/last-N preview, configurable, off
//!   by default
//! - Origin/Referer header check on POST to mitigate DNS rebinding

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
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
    /// Listener address. If unset, `PASTE_BIND` is honored; otherwise
    /// direct CLI use stays localhost-only via `127.0.0.1:0`.
    pub bind_addr: Option<String>,
    /// Public base URL used when Calciforge sits behind a reverse
    /// proxy/tunnel. If unset, `PASTE_PUBLIC_BASE_URL` is honored.
    /// Example: `https://calciforge.example.net/paste-ui`.
    pub public_base_url: Option<String>,
    /// Public hostname/IP used in generated links while keeping the
    /// listener's actual port. If unset, `PASTE_PUBLIC_HOST` is
    /// honored, then Calciforge tries to infer a LAN IP for wildcard
    /// binds before falling back to loopback.
    pub public_host: Option<String>,
}

impl Default for PasteConfig {
    fn default() -> Self {
        Self {
            expiry: Duration::from_secs(5 * 60),
            preview_chars: None,
            require_localhost_origin: true,
            allow_null_origin: false,
            bind_addr: None,
            public_base_url: None,
            public_host: None,
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

/// In-flight bulk-paste request — same lifecycle as a single, but the
/// "name" is an operator-supplied label and the value is a multi-line
/// `KEY=VALUE` dump (`.env` shape).
#[derive(Debug, Clone)]
struct PendingBulkRequest {
    label: String,
    description: String,
    expires_at: chrono::DateTime<chrono::Utc>,
    completed: bool,
}

#[derive(Clone)]
struct ServerState {
    fnox: secrets_client::FnoxClient,
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

#[derive(Clone)]
struct BulkServerState {
    fnox: secrets_client::FnoxClient,
    config: PasteConfig,
    requests: Arc<Mutex<HashMap<String, PendingBulkRequest>>>,
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

/// Spawn a one-shot paste server bound to a random localhost port
/// unless configured otherwise.
/// Returns immediately with the URL the user should open. The server
/// runs in a background tokio task; call [`PasteHandle::wait_submitted`]
/// to block until the user submits, and [`PasteHandle::shutdown`] to
/// trigger graceful drain.
///
/// Port `0` lets the kernel pick a free port. Set
/// [`PasteConfig::bind_addr`] or `PASTE_BIND` when the listener should
/// be reachable from another device.
pub async fn spawn_request(
    name: impl Into<String>,
    description: impl Into<String>,
    fnox: secrets_client::FnoxClient,
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

    let bind_addr = configured_bind_addr(&config);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    let addr: SocketAddr = listener.local_addr()?;
    let url = build_url(&config, addr, "paste", &token);

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

/// Per-line outcome from parsing + storing a bulk paste.
///
/// Each non-blank, non-comment input line maps to exactly one
/// [`BulkLineResult`] in the response — the user sees per-key
/// rejection reasons rather than an opaque "some failed".
#[derive(Debug, Clone)]
pub enum BulkLineResult {
    /// Successfully stored (key + optional preview if enabled).
    Stored {
        key: String,
        preview: Option<String>,
    },
    /// Skipped because the secret already existed and `?update=1`
    /// wasn't passed.
    AlreadyExists { key: String },
    /// Rejected: key contained illegal characters.
    InvalidName { key: String },
    /// Rejected: line wasn't `KEY=value` shape.
    Malformed { line_number: usize, snippet: String },
    /// Storage backend failure when calling fnox set.
    StoreFailed { key: String, error: String },
}

/// Spawn a one-shot **bulk** paste server. Same lifecycle as
/// [`spawn_request`] but the form accepts a multi-line `.env`-shaped
/// dump, and the confirmation page lists per-line results so the
/// user can see exactly which keys stored / which were rejected.
///
/// Use case: typical onboarding starts with a `.env` dump from another
/// project; pasting one-by-one is tedious. With bulk mode a user
/// pastes the whole thing once and sees a checklist back.
///
/// Routes registered on the same `/bulk/:token` namespace (separate
/// from single-secret `/paste/:token`) so the two modes can coexist
/// without a query-param mode switch.
pub async fn spawn_bulk_request(
    label: impl Into<String>,
    description: impl Into<String>,
    fnox: secrets_client::FnoxClient,
    config: PasteConfig,
) -> Result<PasteHandle, PasteError> {
    let label = label.into();
    let token = mint_token();
    let expires_at = chrono::Utc::now()
        + chrono::Duration::from_std(config.expiry).unwrap_or(chrono::Duration::minutes(5));

    let mut state_requests = HashMap::new();
    state_requests.insert(
        token.clone(),
        PendingBulkRequest {
            label: label.clone(),
            description: description.into(),
            expires_at,
            completed: false,
        },
    );

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let (submitted_tx, submitted_rx) = tokio::sync::oneshot::channel::<()>();

    let state = BulkServerState {
        fnox,
        config: config.clone(),
        requests: Arc::new(Mutex::new(state_requests)),
        submitted: Arc::new(Mutex::new(Some(submitted_tx))),
    };

    let app = Router::new()
        .route("/bulk/:token", get(get_bulk_form).post(post_bulk_submit))
        .with_state(state);

    let bind_addr = configured_bind_addr(&config);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    let addr: SocketAddr = listener.local_addr()?;
    let url = build_url(&config, addr, "bulk", &token);

    info!(label = %label, addr = %addr, "secret-paste bulk server listening");
    debug!(label = %label, %url, "secret-paste bulk full URL (debug-only)");

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

/// Parse a `.env`-shaped string into `(key, value)` pairs OR
/// `Malformed` results. Skips blank lines and `#`-prefixed comments.
/// Strips matching surrounding quotes (single or double) on values
/// since `.env` files commonly quote.
///
/// Returns one `Result` per non-blank, non-comment line. Caller
/// interprets `Ok` as "syntactically valid, attempt to store" and
/// `Err` as "give the user a Malformed line result".
fn parse_env_dump(input: &str) -> Vec<Result<(String, String), (usize, String)>> {
    input
        .lines()
        .enumerate()
        .filter_map(|(i, raw)| {
            let line_number = i + 1;
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            // Optional `export KEY=VALUE` prefix that some .env files use
            let body = trimmed.strip_prefix("export ").unwrap_or(trimmed);
            let Some(eq_idx) = body.find('=') else {
                return Some(Err((line_number, body.to_string())));
            };
            let (key, value_with_eq) = body.split_at(eq_idx);
            let value = &value_with_eq[1..]; // strip the '='
            let key = key.trim();
            let value = value.trim();
            // Strip matching quotes — `.env` files often quote values
            // that contain spaces or special chars
            let value = strip_matching_quotes(value);
            if key.is_empty() {
                return Some(Err((line_number, body.to_string())));
            }
            Some(Ok((key.to_string(), value.to_string())))
        })
        .collect()
}

fn configured_bind_addr(config: &PasteConfig) -> String {
    config
        .bind_addr
        .clone()
        .or_else(|| std::env::var("PASTE_BIND").ok())
        .unwrap_or_else(|| "127.0.0.1:0".to_string())
}

fn configured_public_base_url(config: &PasteConfig) -> Option<String> {
    config
        .public_base_url
        .clone()
        .or_else(|| std::env::var("PASTE_PUBLIC_BASE_URL").ok())
        .filter(|s| !s.trim().is_empty())
}

fn configured_public_host(config: &PasteConfig) -> Option<String> {
    config
        .public_host
        .clone()
        .or_else(|| std::env::var("PASTE_PUBLIC_HOST").ok())
        .filter(|s| !s.trim().is_empty())
}

fn build_url(config: &PasteConfig, addr: SocketAddr, route: &str, token: &str) -> String {
    let path = format!("{route}/{token}");
    if let Some(base) = configured_public_base_url(config) {
        return format!("{}/{}", base.trim_end_matches('/'), path);
    }

    let host = configured_public_host(config).unwrap_or_else(|| public_host_for_addr(addr.ip()));
    let host = bracket_ipv6_host(&host);
    format!("http://{host}:{}/{path}", addr.port())
}

fn public_host_for_addr(ip: IpAddr) -> String {
    if ip.is_unspecified() {
        if let Some(lan_ip) = detect_lan_ip() {
            return lan_ip.to_string();
        }
        return "127.0.0.1".to_string();
    }
    ip.to_string()
}

fn bracket_ipv6_host(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') && !host.contains("://") {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

fn detect_lan_ip() -> Option<IpAddr> {
    for target in ["192.0.2.1:80", "198.51.100.1:80", "203.0.113.1:80"] {
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        if socket.connect(target).is_err() {
            continue;
        }
        let ip = socket.local_addr().ok()?.ip();
        if !ip.is_loopback() && !ip.is_unspecified() {
            return Some(ip);
        }
    }
    None
}

fn strip_matching_quotes(value: &str) -> &str {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

async fn get_bulk_form(
    State(state): State<BulkServerState>,
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
            Html("This bulk-paste link has already been used.".to_string()),
        )
            .into_response();
    }
    Html(render_bulk_form(&req.label, &req.description, &token)).into_response()
}

#[derive(Deserialize)]
struct BulkSubmitForm {
    dump: String,
}

async fn post_bulk_submit(
    State(state): State<BulkServerState>,
    Path(token): Path<String>,
    Query(query): Query<UpdateQuery>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<BulkSubmitForm>,
) -> impl IntoResponse {
    if state.config.require_localhost_origin {
        let allow_null = state.config.allow_null_origin;
        let ok = headers
            .get(axum::http::header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|o| is_allowed_origin(o, &state.config, allow_null));
        if !ok {
            warn!("rejecting bulk POST: missing/untrusted Origin header");
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
            Html("This bulk-paste link has already been used.".to_string()),
        )
            .into_response();
    }
    if form.dump.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Html("Empty dump rejected.".to_string()),
        )
            .into_response();
    }

    let allow_update = query.update.unwrap_or(0) != 0;
    // Pre-fetch the existing key set ONCE rather than calling fnox list
    // per-line. Failure to fetch is fatal — we'd rather refuse the whole
    // bulk than silently overwrite.
    let existing: std::collections::HashSet<String> = if allow_update {
        std::collections::HashSet::new()
    } else {
        match state.fnox.list().await {
            Ok(names) => names.into_iter().collect(),
            Err(e) => {
                warn!("fnox list failed during bulk new-only check: {e}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Html("Failed to verify existing secret state — refusing bulk set.".to_string()),
                )
                    .into_response();
            }
        }
    };

    let parsed = parse_env_dump(&form.dump);
    let mut results: Vec<BulkLineResult> = Vec::with_capacity(parsed.len());
    let mut any_stored = false;
    for entry in parsed {
        match entry {
            Err((line_number, snippet)) => results.push(BulkLineResult::Malformed {
                line_number,
                snippet,
            }),
            Ok((key, value)) => {
                if !is_valid_name(&key) {
                    results.push(BulkLineResult::InvalidName { key });
                    continue;
                }
                if !allow_update && existing.contains(&key) {
                    results.push(BulkLineResult::AlreadyExists { key });
                    continue;
                }
                match state.fnox.set(&key, &value).await {
                    Ok(()) => {
                        let preview = state
                            .config
                            .preview_chars
                            .map(|n| truncated_preview(&value, n));
                        results.push(BulkLineResult::Stored { key, preview });
                        any_stored = true;
                    }
                    Err(e) => {
                        results.push(BulkLineResult::StoreFailed {
                            key,
                            error: e.to_string(),
                        });
                    }
                }
            }
        }
    }

    {
        let mut map = state.requests.lock().await;
        if let Some(r) = map.get_mut(&token) {
            r.completed = true;
        }
    }
    // Signal submitted only if at least one key actually landed —
    // otherwise the operator probably wants the URL to stay live so
    // they can fix the dump and retry. (Server still marks completed
    // so the same token can't be reused; they'd request a new bulk URL.)
    if any_stored && let Some(tx) = state.submitted.lock().await.take() {
        let _ = tx.send(());
    }

    Html(render_bulk_results(&req.label, &results)).into_response()
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
            .is_some_and(|o| is_allowed_origin(o, &state.config, allow_null));
        if !ok {
            warn!("rejecting paste POST: missing/untrusted Origin header");
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

fn is_allowed_origin(origin: &str, config: &PasteConfig, allow_null: bool) -> bool {
    if origin == "null" {
        return allow_null;
    }

    if let Some(base) = configured_public_base_url(config)
        && origin_matches_base_url(origin, &base)
    {
        return true;
    }

    if let Some(host) = configured_public_host(config)
        && origin_matches_public_host(origin, &host)
    {
        return true;
    }

    is_localhost_origin(origin)
}

fn is_localhost_origin(origin: &str) -> bool {
    if origin.starts_with("http://127.0.0.1:")
        || origin.starts_with("http://localhost:")
        || origin == "http://localhost"
    {
        return true;
    }
    // Accept RFC 1918 private ranges so a phone or another machine on
    // the same LAN can submit. Treats the home network as trusted —
    // the check still blocks origins from the public internet.
    is_rfc1918_http_origin(origin)
}

#[derive(Debug, PartialEq, Eq)]
struct OriginParts<'a> {
    scheme: &'a str,
    host: &'a str,
    port: Option<u16>,
}

fn parse_origin_like(input: &str) -> Option<OriginParts<'_>> {
    let (scheme, rest) = input.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }

    let authority = rest.split('/').next().unwrap_or(rest);
    if authority.is_empty() || authority.contains('@') {
        return None;
    }

    let (host, port) = if let Some(after_bracket) = authority.strip_prefix('[') {
        let (host, remainder) = after_bracket.split_once(']')?;
        let port = if let Some(port_text) = remainder.strip_prefix(':') {
            Some(port_text.parse().ok()?)
        } else if remainder.is_empty() {
            None
        } else {
            return None;
        };
        (host, port)
    } else {
        let mut pieces = authority.split(':');
        let host = pieces.next()?;
        let port = match pieces.next() {
            Some(port_text) => Some(port_text.parse().ok()?),
            None => None,
        };
        if pieces.next().is_some() {
            return None;
        }
        (host, port)
    };

    if host.is_empty() {
        return None;
    }
    Some(OriginParts { scheme, host, port })
}

fn origin_matches_base_url(origin: &str, base_url: &str) -> bool {
    let Some(origin) = parse_origin_like(origin) else {
        return false;
    };
    let Some(base) = parse_origin_like(base_url) else {
        return false;
    };
    origin.scheme == base.scheme
        && origin.host.eq_ignore_ascii_case(base.host)
        && origin.port == base.port
}

fn origin_matches_public_host(origin: &str, public_host: &str) -> bool {
    let Some(origin) = parse_origin_like(origin) else {
        return false;
    };
    let normalized = public_host
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']');
    origin.host.eq_ignore_ascii_case(normalized)
}

fn is_rfc1918_http_origin(origin: &str) -> bool {
    let Some(rest) = origin.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(':').next().unwrap_or("");
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    let octets: Option<Vec<u8>> = parts.iter().map(|p| p.parse::<u8>().ok()).collect();
    let Some(o) = octets else { return false };
    // 10.0.0.0/8
    if o[0] == 10 {
        return true;
    }
    // 172.16.0.0/12
    if o[0] == 172 && (16..=31).contains(&o[1]) {
        return true;
    }
    // 192.168.0.0/16
    if o[0] == 192 && o[1] == 168 {
        return true;
    }
    false
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

fn render_bulk_form(label: &str, description: &str, token: &str) -> String {
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Bulk paste — {label}</title>
<style>body {{font-family:system-ui,sans-serif;max-width:720px;margin:3rem auto;padding:0 1rem;color:#1a1a1a}}
h1 {{font-size:1.2rem}} textarea {{width:100%;min-height:280px;padding:.6rem;font-family:ui-monospace,Menlo,monospace;font-size:.95rem;border:1px solid #888;border-radius:4px}}
button {{margin-top:1rem;padding:.6rem 1.2rem;font-size:1rem;border:0;border-radius:4px;background:#0a6;color:#fff;cursor:pointer}}
.warn {{background:#ffe;border:1px solid #cc8;padding:.6rem;border-radius:4px;font-size:.9rem;margin-top:1rem}}
.help {{color:#555;font-size:.9rem;margin:.4rem 0 .8rem}}</style>
</head><body>
<h1>Bulk-set secrets — <code>{label}</code></h1>
<p>{description}</p>
<form method="POST" action="/bulk/{token}">
<label>Paste a <code>.env</code>-style dump. One <code>KEY=VALUE</code> per line. Comments (<code>#</code>) and blank lines are ignored. <code>export KEY=…</code> prefixes are stripped. Surrounding quotes are stripped from values.</label>
<p class="help">By default, existing secrets are NOT overwritten — they'll appear in the result page as <em>already exists</em>. To intentionally rotate, append <code>?update=1</code> to this URL.</p>
<textarea name="dump" autofocus required placeholder="DATABASE_URL=postgres://localhost/app
NPM_TOKEN=npm_aBcD1234
# this line is a comment, ignored
export STRIPE_KEY=&quot;sk_live_with spaces&quot;"></textarea>
<button type="submit">Store all</button>
</form>
<div class="warn">⚠ This URL is single-use and expires shortly. The full dump is processed once and never displayed.</div>
</body></html>"#,
        label = html_escape(label),
        description = html_escape(description),
        token = html_escape(token),
    )
}

fn render_bulk_results(label: &str, results: &[BulkLineResult]) -> String {
    let mut stored = 0;
    let mut existed = 0;
    let mut invalid = 0;
    let mut malformed = 0;
    let mut failed = 0;
    let mut rows = String::new();
    for r in results {
        match r {
            BulkLineResult::Stored { key, preview } => {
                stored += 1;
                let preview_html = preview
                    .as_ref()
                    .map(|p| format!(" <span style=\"color:#666\">({})</span>", html_escape(p)))
                    .unwrap_or_default();
                rows.push_str(&format!(
                    "<li class=\"ok\">✓ stored <code>{}</code>{}</li>",
                    html_escape(key),
                    preview_html
                ));
            }
            BulkLineResult::AlreadyExists { key } => {
                existed += 1;
                rows.push_str(&format!(
                    "<li class=\"skip\">⊘ <code>{}</code> already exists \
                     <span style=\"color:#666\">(re-open with <code>?update=1</code> to overwrite)</span></li>",
                    html_escape(key)
                ));
            }
            BulkLineResult::InvalidName { key } => {
                invalid += 1;
                rows.push_str(&format!(
                    "<li class=\"err\">✗ <code>{}</code> rejected — illegal characters in key (allowed: A-Z a-z 0-9 _ -)</li>",
                    html_escape(key)
                ));
            }
            BulkLineResult::Malformed {
                line_number,
                snippet,
            } => {
                malformed += 1;
                rows.push_str(&format!(
                    "<li class=\"err\">✗ line {} not <code>KEY=VALUE</code>: <code>{}</code></li>",
                    line_number,
                    html_escape(snippet)
                ));
            }
            BulkLineResult::StoreFailed { key, error } => {
                failed += 1;
                rows.push_str(&format!(
                    "<li class=\"err\">✗ <code>{}</code> backend error: {}</li>",
                    html_escape(key),
                    html_escape(error)
                ));
            }
        }
    }
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Bulk results — {label}</title>
<style>body {{font-family:system-ui,sans-serif;max-width:720px;margin:3rem auto;padding:0 1rem;color:#1a1a1a}}
h1 {{font-size:1.2rem}} ul {{padding-left:1.2rem;line-height:1.7}}
.ok {{color:#0a6}} .skip {{color:#a60}} .err {{color:#a00}}
.summary {{background:#f4f4f4;padding:.6rem .8rem;border-radius:4px;font-size:.95rem;margin-bottom:1rem}}</style>
</head><body>
<h1>Bulk results — <code>{label}</code></h1>
<div class="summary">
  <strong>{stored}</strong> stored ·
  <strong>{existed}</strong> already-exists ·
  <strong>{invalid}</strong> invalid name ·
  <strong>{malformed}</strong> malformed ·
  <strong>{failed}</strong> backend failure
</div>
<ul>{rows}</ul>
<p style="color:#666">You can close this tab. The bulk-paste URL is now spent.</p>
</body></html>"#,
        label = html_escape(label),
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
    fn display_url_uses_public_host_for_wildcard_bind() {
        let cfg = PasteConfig {
            public_host: Some("192.168.1.55".to_string()),
            ..PasteConfig::default()
        };
        let addr: SocketAddr = "0.0.0.0:58083".parse().unwrap();

        let url = build_url(&cfg, addr, "bulk", "abc123");

        assert_eq!(url, "http://192.168.1.55:58083/bulk/abc123");
    }

    #[test]
    fn display_url_uses_public_base_url_for_proxy_deployments() {
        let cfg = PasteConfig {
            public_base_url: Some("https://calciforge.example.net/secret-paste/".to_string()),
            ..PasteConfig::default()
        };
        let addr: SocketAddr = "127.0.0.1:58083".parse().unwrap();

        let url = build_url(&cfg, addr, "paste", "abc123");

        assert_eq!(
            url,
            "https://calciforge.example.net/secret-paste/paste/abc123"
        );
    }

    #[test]
    fn display_url_never_returns_unspecified_host() {
        let cfg = PasteConfig::default();
        let addr: SocketAddr = "0.0.0.0:58083".parse().unwrap();

        let url = build_url(&cfg, addr, "bulk", "abc123");

        assert!(
            !url.contains("0.0.0.0"),
            "wildcard bind address is not a usable browser URL: {url}"
        );
    }

    #[test]
    fn display_url_keeps_loopback_bind_local() {
        let cfg = PasteConfig::default();
        let addr: SocketAddr = "127.0.0.1:58083".parse().unwrap();

        let url = build_url(&cfg, addr, "paste", "abc123");

        assert_eq!(url, "http://127.0.0.1:58083/paste/abc123");
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
        let client = secrets_client::FnoxClient::with_binary(bin);

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
        let client = secrets_client::FnoxClient::with_binary(bin);

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
        let client = secrets_client::FnoxClient::with_binary(bin);

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
        let client = secrets_client::FnoxClient::with_binary(bin);

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
        let client = secrets_client::FnoxClient::with_binary(bin);

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

    /// Pure-function checks for the widened Origin predicate. Locked in
    /// so a future "tighten the predicate" change has to confront the
    /// LAN use case head-on, not silently break it.
    #[test]
    fn origin_predicate_accepts_loopback_and_rfc1918_rejects_public() {
        // loopback
        assert!(is_localhost_origin("http://127.0.0.1:8080"));
        assert!(is_localhost_origin("http://localhost:8080"));
        // RFC 1918 ranges
        assert!(is_localhost_origin("http://192.168.1.175:58083"));
        assert!(is_localhost_origin("http://10.0.0.5:9000"));
        assert!(is_localhost_origin("http://172.16.5.1:80"));
        assert!(is_localhost_origin("http://172.31.255.1:80"));
        // boundary: 172.32 is NOT private
        assert!(!is_localhost_origin("http://172.32.0.1:80"));
        // public
        assert!(!is_localhost_origin("http://example.com"));
        assert!(!is_localhost_origin("http://8.8.8.8:80"));
        // CGNAT-adjacent — explicitly NOT in the allowed set
        assert!(!is_localhost_origin("http://100.64.0.1:80"));
        // https not http (paste server is plain http; https can't have come from us)
        assert!(!is_localhost_origin("https://192.168.1.1:443"));
        // null still gated by allow_null_origin
        assert!(!is_allowed_origin("null", &PasteConfig::default(), false));
        assert!(is_allowed_origin("null", &PasteConfig::default(), true));
    }

    #[test]
    fn origin_predicate_accepts_configured_public_base_url_origin() {
        let cfg = PasteConfig {
            public_base_url: Some("https://calciforge.example.net/secret-paste".to_string()),
            ..PasteConfig::default()
        };

        assert!(is_allowed_origin(
            "https://calciforge.example.net",
            &cfg,
            false
        ));
        assert!(!is_allowed_origin(
            "http://calciforge.example.net",
            &cfg,
            false
        ));
        assert!(!is_allowed_origin(
            "https://attacker.example.net",
            &cfg,
            false
        ));
    }

    #[test]
    fn origin_predicate_accepts_configured_public_host_origin() {
        let cfg = PasteConfig {
            public_host: Some("calciforge.local".to_string()),
            ..PasteConfig::default()
        };

        assert!(is_allowed_origin(
            "http://calciforge.local:58083",
            &cfg,
            false
        ));
        assert!(is_allowed_origin("https://calciforge.local", &cfg, false));
        assert!(!is_allowed_origin("http://other.local:58083", &cfg, false));
    }

    /// Given a POST whose Origin is an RFC 1918 LAN IP (192.168.x.y),
    /// when the user POSTs (default config),
    /// then the server accepts the Origin and proceeds to fnox.
    /// Regression guard: this used to be 403 because the predicate was
    /// loopback-only. The widened-defaults change must keep this 200.
    #[tokio::test]
    async fn rfc1918_origin_accepted_by_default() {
        let dir = TempDir::new().unwrap();
        let bin = fake_fnox(
            &dir,
            r#"if [ "$1" = "list" ]; then exit 0; else cat > /dev/null; exit 0; fi"#,
        );
        let client = secrets_client::FnoxClient::with_binary(bin);
        let handle = spawn_request("K", "", client, PasteConfig::default())
            .await
            .unwrap();

        let http = reqwest::Client::new();
        let resp = http
            .post(&handle.url)
            .header("Origin", "http://192.168.1.175:8080")
            .form(&[("value", "v")])
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            200,
            "RFC 1918 Origin must be accepted by default — phone-on-LAN flow"
        );
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
        let client = secrets_client::FnoxClient::with_binary(bin);

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
        let client = secrets_client::FnoxClient::with_binary(bin);

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
        let client = secrets_client::FnoxClient::with_binary(bin);

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

    // ── Bulk-paste tests ──────────────────────────────────────────────

    /// Pure-function test: the .env parser should skip blanks and
    /// `#` comments, accept `export KEY=…` prefixes, and strip
    /// surrounding quotes. Locks the contract so future "let's also
    /// support inline comments" changes have to confront the existing
    /// shape.
    #[test]
    fn parse_env_dump_handles_comments_blanks_export_quotes() {
        let input = r#"
# leading comment
NPM_TOKEN=npm_abc123

export DATABASE_URL="postgres://localhost/app"
STRIPE='sk_with_quotes'
# another comment
INVALID_NO_EQUALS
=NO_KEY
"#;
        let out = parse_env_dump(input);
        // Expect 5 results (NPM_TOKEN ok, DATABASE_URL ok, STRIPE ok,
        // INVALID_NO_EQUALS malformed, =NO_KEY malformed)
        assert_eq!(out.len(), 5, "got: {out:?}");
        assert!(
            matches!(&out[0], Ok((k, v)) if k == "NPM_TOKEN" && v == "npm_abc123"),
            "{out:?}"
        );
        assert!(
            matches!(&out[1], Ok((k, v)) if k == "DATABASE_URL" && v == "postgres://localhost/app"),
            "export prefix + double quotes; got {out:?}"
        );
        assert!(
            matches!(&out[2], Ok((k, v)) if k == "STRIPE" && v == "sk_with_quotes"),
            "single quotes; got {out:?}"
        );
        assert!(
            matches!(out[3], Err((_, _))),
            "no = should be Err; got {out:?}"
        );
        assert!(
            matches!(out[4], Err((_, _))),
            "empty key should be Err; got {out:?}"
        );
    }

    /// Given a fresh bulk URL and a 3-line dump (one new, one
    /// existing, one with illegal chars),
    /// when the user POSTs,
    /// then exactly one fnox set fires (the new key) AND the
    /// confirmation page lists per-line outcomes:
    ///   ✓ NEW_KEY stored
    ///   ⊘ EXISTS_KEY already exists
    ///   ✗ BAD KEY illegal characters
    #[tokio::test]
    async fn bulk_post_partitions_results_per_line() {
        let dir = TempDir::new().unwrap();
        // Fake fnox: list returns one existing key, set succeeds.
        let bin = fake_fnox(
            &dir,
            r#"if [ "$1" = "list" ]; then echo "EXISTS_KEY"; exit 0; else cat > /dev/null; exit 0; fi"#,
        );
        let client = secrets_client::FnoxClient::with_binary(bin);

        let mut handle = spawn_bulk_request(
            "onboarding-batch",
            "test bulk",
            client,
            PasteConfig {
                require_localhost_origin: false,
                ..PasteConfig::default()
            },
        )
        .await
        .unwrap();
        let url = handle.url.clone();

        let dump = "NEW_KEY=value1\nEXISTS_KEY=value2\nBAD KEY=value3\n";
        let http = reqwest::Client::new();
        let resp = http
            .post(&url)
            .form(&[("dump", dump)])
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = resp.text().await.unwrap();

        // Per-line results all visible
        assert!(
            body.contains("stored") && body.contains("NEW_KEY"),
            "body: {body}"
        );
        assert!(
            body.contains("already exists") && body.contains("EXISTS_KEY"),
            "body: {body}"
        );
        assert!(
            body.contains("illegal characters") && body.contains("BAD KEY"),
            "body: {body}"
        );
        // Summary counts present
        assert!(body.contains("1</strong> stored"), "body: {body}");
        assert!(body.contains("1</strong> already-exists"), "body: {body}");

        handle.shutdown();
    }

    /// Given a bulk URL with `?update=1`,
    /// when the user POSTs a key that already exists,
    /// then it stores anyway (no list call) and reports stored.
    /// Locks the contract that update=1 explicitly opts into rotation.
    #[tokio::test]
    async fn bulk_update_query_rotates_existing_keys() {
        let dir = TempDir::new().unwrap();
        let bin = fake_fnox(
            &dir,
            // list would return EXISTS_KEY but update=1 should never call it
            r#"if [ "$1" = "list" ]; then exit 1; else cat > /dev/null; exit 0; fi"#,
        );
        let client = secrets_client::FnoxClient::with_binary(bin);

        let mut handle = spawn_bulk_request(
            "rotation",
            "",
            client,
            PasteConfig {
                require_localhost_origin: false,
                ..PasteConfig::default()
            },
        )
        .await
        .unwrap();
        // Append ?update=1 to the URL the spawn produced
        let url = format!("{}?update=1", handle.url);

        let http = reqwest::Client::new();
        let resp = http
            .post(&url)
            .form(&[("dump", "EXISTS_KEY=new_value")])
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = resp.text().await.unwrap();
        assert!(
            body.contains("stored") && body.contains("EXISTS_KEY"),
            "update=1 must skip existence check; body: {body}"
        );

        handle.shutdown();
    }

    /// Given a bulk URL and an empty dump,
    /// when the user POSTs,
    /// then 400 — we don't burn the token on an empty submission.
    #[tokio::test]
    async fn bulk_empty_dump_is_400() {
        let dir = TempDir::new().unwrap();
        let bin = fake_fnox(&dir, "exit 0");
        let client = secrets_client::FnoxClient::with_binary(bin);

        let handle = spawn_bulk_request(
            "empty-test",
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
            .form(&[("dump", "")])
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
    }
}
