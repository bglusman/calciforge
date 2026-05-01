//! OpenClawChannelAdapter — bridge Calciforge to the OpenClaw calciforge plugin.
//!
//! This adapter posts inbound messages to OpenClaw at
//! `POST /calciforge/inbound` (gateway auth) and waits for a correlated callback on the
//! local reply webhook `POST /hooks/reply`.

use std::collections::HashMap;
use std::error::Error as StdError;
use std::time::Duration;

use crate::artifacts::{create_run_dir, write_inline_attachment};
use crate::messages::OutboundMessage;
use crate::sync::{Arc, AtomicBool, OnceLock, Ordering};

use async_trait::async_trait;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, Mutex, Notify};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::{AdapterError, AgentAdapter, DispatchContext};

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const DEFAULT_REPLY_PORT: u16 = 18_797;
const OPENCLAW_ARTIFACT_ROOT_NAME: &str = "calciforge-openclaw-artifacts";
const MAX_REPLY_ATTACHMENTS: usize = 8;
const MAX_REPLY_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;
const INBOUND_CONNECT_RETRY_DELAYS_MS: [u64; 2] = [250, 1_000];

type ReplyResult = Result<OutboundMessage, String>;

#[derive(Clone)]
struct PendingReply {
    request_id: String,
    session_key: String,
    tx: Arc<Mutex<Option<oneshot::Sender<ReplyResult>>>>,
}

/// Correlates OpenClaw callbacks to pending dispatch requests.
#[derive(Clone, Default)]
pub struct ReplyRouter {
    pending: Arc<Mutex<HashMap<String, PendingReply>>>,
}

impl ReplyRouter {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn insert(
        &self,
        request_id: String,
        session_key: String,
        tx: oneshot::Sender<ReplyResult>,
    ) {
        let entry = PendingReply {
            request_id: request_id.clone(),
            session_key: session_key.clone(),
            tx: Arc::new(Mutex::new(Some(tx))),
        };
        let mut pending = self.pending.lock().await;
        let has_other_pending_for_session = pending.values().any(|candidate| {
            candidate.session_key == session_key && candidate.request_id != request_id
        });
        pending.insert(request_id, entry.clone());
        if has_other_pending_for_session {
            // Legacy callbacks only carry the stable session key. Once more
            // than one request is pending for that session, routing by
            // session key can cross-deliver replies, so fail closed for the
            // legacy path until requestId-capable plugins are installed.
            pending.remove(&session_key);
        } else {
            pending.insert(session_key, entry);
        }
    }

    pub async fn take(&self, correlation_key: &str) -> Option<oneshot::Sender<ReplyResult>> {
        let entry = {
            let mut pending = self.pending.lock().await;
            let entry = pending.remove(correlation_key)?;
            pending.retain(|_, candidate| candidate.request_id != entry.request_id);
            entry
        };

        let tx = entry.tx.lock().await.take();
        tx
    }

    pub async fn remove(&self, request_id: &str) {
        self.pending
            .lock()
            .await
            .retain(|_, entry| entry.request_id != request_id);
    }
}

#[derive(Clone)]
struct ReplyServerState {
    router: ReplyRouter,
    auth_token: Option<String>,
}

/// Reply webhook body sent by the OpenClaw plugin.
#[derive(Debug, Clone, Deserialize)]
struct ReplyPayload {
    #[serde(rename = "sessionKey")]
    session_key: String,
    #[serde(default, rename = "requestId")]
    request_id: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    attachments: Vec<ReplyAttachmentPayload>,
    #[allow(dead_code)]
    channel: Option<String>,
    #[allow(dead_code)]
    to: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ReplyAttachmentPayload {
    /// Optional display filename. Calciforge sanitizes this before writing.
    #[serde(default)]
    name: Option<String>,
    /// MIME type supplied by the agent bridge, for example `image/png`.
    #[serde(default, rename = "mimeType", alias = "mime_type")]
    mime_type: Option<String>,
    /// Optional caption used by channel fallback renderers.
    #[serde(default)]
    caption: Option<String>,
    /// Inline base64 data. URL fetching is intentionally not implemented here;
    /// it needs a separate SSRF-safe policy surface.
    #[serde(default, rename = "dataBase64", alias = "data_base64")]
    data_base64: Option<String>,
}

#[derive(Debug, Serialize)]
struct AckResponse {
    ok: bool,
}

/// Local reply server that receives `POST /hooks/reply` callbacks.
struct ReplyServer;

impl ReplyServer {
    async fn run(
        port: u16,
        state: ReplyServerState,
        ready_tx: oneshot::Sender<Result<(), String>>,
    ) {
        let app = Router::new()
            .route("/hooks/reply", post(handle_reply))
            .with_state(state);

        let listener = match TcpListener::bind(("0.0.0.0", port)).await {
            Ok(l) => l,
            Err(e) => {
                let _ = ready_tx.send(Err(format!("bind 0.0.0.0:{port} failed: {e}")));
                return;
            }
        };

        let _ = ready_tx.send(Ok(()));
        if let Err(e) = axum::serve(listener, app).await {
            error!(error = %e, port, "openclaw-channel reply server stopped");
        }
    }
}

#[derive(Clone)]
struct SharedReplyServer {
    port: u16,
    auth_token: Option<String>,
    router: ReplyRouter,
    once: Arc<OnceLock<()>>,
    ready_notify: Arc<Notify>,
    startup_complete: Arc<AtomicBool>,
    started: Arc<AtomicBool>,
    start_error: Arc<Mutex<Option<String>>>,
}

#[derive(Clone)]
struct ReplyServerHandle {
    shared: SharedReplyServer,
    config_error: Option<String>,
}

impl ReplyServerHandle {
    fn for_port(port: u16, auth_token: Option<String>) -> Self {
        static REPLY_SERVERS: OnceLock<std::sync::Mutex<HashMap<u16, SharedReplyServer>>> =
            OnceLock::new();

        let registry = REPLY_SERVERS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
        let mut servers = registry
            .lock()
            .expect("openclaw-channel reply server registry poisoned");

        if let Some(existing) = servers.get(&port) {
            let config_error = if existing.auth_token == auth_token {
                None
            } else {
                Some(format!(
                    "reply port {port} is already registered with a different reply_auth_token"
                ))
            };
            return Self {
                shared: existing.clone(),
                config_error,
            };
        }

        let shared = SharedReplyServer {
            port,
            auth_token,
            router: ReplyRouter::new(),
            once: Arc::new(OnceLock::new()),
            ready_notify: Arc::new(Notify::new()),
            startup_complete: Arc::new(AtomicBool::new(false)),
            started: Arc::new(AtomicBool::new(false)),
            start_error: Arc::new(Mutex::new(None)),
        };
        servers.insert(port, shared.clone());

        Self {
            shared,
            config_error: None,
        }
    }
}

async fn handle_reply(
    State(state): State<ReplyServerState>,
    headers: HeaderMap,
    Json(payload): Json<ReplyPayload>,
) -> (StatusCode, Json<AckResponse>) {
    if let Some(expected) = state.auth_token.as_deref() {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let token = auth.strip_prefix("Bearer ").unwrap_or(auth);
        if token != expected {
            return (StatusCode::UNAUTHORIZED, Json(AckResponse { ok: false }));
        }
    }

    let correlation_key = payload
        .request_id
        .as_deref()
        .filter(|request_id| !request_id.trim().is_empty())
        .unwrap_or(&payload.session_key)
        .to_string();

    if let Some(tx) = state.router.take(&correlation_key).await {
        match payload.into_outbound_message() {
            Ok(message) => {
                let _ = tx.send(Ok(message));
                (StatusCode::OK, Json(AckResponse { ok: true }))
            }
            Err(e) => {
                let _ = tx.send(Err(e));
                (StatusCode::BAD_REQUEST, Json(AckResponse { ok: false }))
            }
        }
    } else {
        warn!(
            session_key = %payload.session_key,
            correlation_key = %correlation_key,
            "openclaw-channel reply without pending request"
        );
        (StatusCode::ACCEPTED, Json(AckResponse { ok: true }))
    }
}

impl ReplyPayload {
    fn into_outbound_message(self) -> Result<OutboundMessage, String> {
        if self.attachments.len() > MAX_REPLY_ATTACHMENTS {
            return Err(format!(
                "openclaw-channel callback included {} attachments, limit is {}",
                self.attachments.len(),
                MAX_REPLY_ATTACHMENTS
            ));
        }

        let run_dir = if self.attachments.is_empty() {
            None
        } else {
            Some(create_run_dir(OPENCLAW_ARTIFACT_ROOT_NAME)?)
        };

        let mut attachments = Vec::with_capacity(self.attachments.len());
        let run_dir = run_dir.as_deref();
        for (index, attachment) in self.attachments.into_iter().enumerate() {
            let run_dir = run_dir.ok_or_else(|| {
                "openclaw-channel callback attachment storage was not initialized".to_string()
            })?;
            attachments.push(attachment.into_outbound_attachment(run_dir, index)?);
        }

        Ok(OutboundMessage {
            text: self.message.filter(|message| !message.trim().is_empty()),
            attachments,
            controls: Vec::new(),
        })
    }
}

impl ReplyAttachmentPayload {
    fn into_outbound_attachment(
        self,
        run_dir: &std::path::Path,
        index: usize,
    ) -> Result<crate::messages::OutboundAttachment, String> {
        let data_base64 = self
            .data_base64
            .ok_or_else(|| "openclaw-channel callback attachment missing dataBase64".to_string())?;

        write_inline_attachment(
            run_dir,
            index,
            self.name.as_deref(),
            self.mime_type.as_deref(),
            self.caption,
            &data_base64,
            MAX_REPLY_ATTACHMENT_BYTES,
        )
        .map_err(|e| format!("openclaw-channel {e}"))
    }
}

#[derive(Debug, Serialize)]
struct InboundPayload<'a> {
    message: &'a str,
    #[serde(rename = "sessionKey")]
    session_key: String,
    #[serde(rename = "requestId")]
    request_id: String,
    sender: &'a str,
    #[serde(rename = "channel")]
    channel: Option<&'a str>,
    #[serde(rename = "replyTo")]
    reply_to: Option<&'a str>,
    #[serde(rename = "agentId")]
    agent_id: &'a str,
}

pub struct OpenClawChannelAdapter {
    client: reqwest::Client,
    endpoint: String,
    auth_token: String,
    openclaw_agent_id: String,
    timeout: Duration,
    reply_server: ReplyServerHandle,
}

impl OpenClawChannelAdapter {
    pub fn new(
        endpoint: String,
        auth_token: String,
        openclaw_agent_id: String,
        reply_port: Option<u16>,
        reply_auth_token: Option<String>,
        timeout_ms: Option<u64>,
    ) -> Self {
        let timeout = Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(timeout)
            .no_proxy()
            .build()
            .expect("reqwest client");

        let reply_server =
            ReplyServerHandle::for_port(reply_port.unwrap_or(DEFAULT_REPLY_PORT), reply_auth_token);

        Self {
            client,
            endpoint,
            auth_token,
            openclaw_agent_id,
            timeout,
            reply_server,
        }
    }

    fn inbound_url(&self) -> String {
        format!("{}/calciforge/inbound", self.endpoint.trim_end_matches('/'))
    }

    fn session_key_for(&self, sender: &str) -> String {
        format!("calciforge:{}:{}", self.openclaw_agent_id, sender)
    }

    async fn ensure_reply_server_started(&self) -> Result<(), AdapterError> {
        if let Some(err) = self.reply_server.config_error.as_deref() {
            return Err(AdapterError::Unavailable(err.to_string()));
        }

        let shared = &self.reply_server.shared;
        if shared.once.set(()).is_ok() {
            let (ready_tx, ready_rx) = oneshot::channel::<Result<(), String>>();
            let state = ReplyServerState {
                router: shared.router.clone(),
                auth_token: shared.auth_token.clone(),
            };
            let port = shared.port;

            tokio::spawn(async move {
                ReplyServer::run(port, state, ready_tx).await;
            });

            let startup_result = ready_rx
                .await
                .unwrap_or_else(|_| Err("reply server startup channel dropped".to_string()));

            match startup_result {
                Ok(()) => {
                    shared.started.store(true, Ordering::SeqCst);
                    info!(port, "openclaw-channel reply server started");
                }
                Err(e) => {
                    *shared.start_error.lock().await = Some(e);
                }
            }
            shared.startup_complete.store(true, Ordering::SeqCst);
            shared.ready_notify.notify_waiters();
        } else if !shared.startup_complete.load(Ordering::SeqCst) {
            let notified = shared.ready_notify.notified();
            if !shared.startup_complete.load(Ordering::SeqCst) {
                notified.await;
            }
        }

        if let Some(err) = shared.start_error.lock().await.clone() {
            return Err(AdapterError::Unavailable(format!(
                "openclaw-channel reply server failed to start: {}",
                err
            )));
        }

        Ok(())
    }
}

#[async_trait]
impl AgentAdapter for OpenClawChannelAdapter {
    async fn dispatch(&self, msg: &str) -> Result<String, AdapterError> {
        self.dispatch_with_context(DispatchContext::message_only(msg))
            .await
    }

    async fn dispatch_with_context(
        &self,
        ctx: DispatchContext<'_>,
    ) -> Result<String, AdapterError> {
        self.dispatch_message_with_context(ctx)
            .await
            .map(|message| message.render_text_fallback())
    }

    async fn dispatch_message_with_context(
        &self,
        ctx: DispatchContext<'_>,
    ) -> Result<OutboundMessage, AdapterError> {
        self.ensure_reply_server_started().await?;

        let sender = ctx.sender.unwrap_or("unknown");
        let session_key = self.session_key_for(sender);
        let request_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel::<ReplyResult>();
        self.reply_server
            .shared
            .router
            .insert(request_id.clone(), session_key.clone(), tx)
            .await;

        let body = InboundPayload {
            message: ctx.message,
            session_key: session_key.clone(),
            request_id: request_id.clone(),
            sender,
            channel: ctx.channel,
            reply_to: None,
            agent_id: &self.openclaw_agent_id,
        };

        let url = self.inbound_url();
        debug!(
            endpoint = %url,
            sender,
            session_key = %session_key,
            request_id = %request_id,
            "openclaw-channel dispatch"
        );

        let inbound_resp =
            send_inbound_with_retries(&self.client, &url, &self.auth_token, &body).await;

        let inbound_resp = match inbound_resp {
            Ok(r) => r,
            Err(e) => {
                self.reply_server.shared.router.remove(&request_id).await;
                return Err(e);
            }
        };

        if !inbound_resp.status().is_success() {
            let status = inbound_resp.status();
            let body = inbound_resp.text().await.unwrap_or_default();
            self.reply_server.shared.router.remove(&request_id).await;
            return Err(AdapterError::Protocol(format!(
                "openclaw-channel inbound HTTP {}: {}",
                status, body
            )));
        }

        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(Ok(reply))) => Ok(reply),
            Ok(Ok(Err(e))) => Err(AdapterError::Protocol(e)),
            Ok(Err(_)) => {
                self.reply_server.shared.router.remove(&request_id).await;
                Err(AdapterError::Protocol(
                    "openclaw-channel reply correlation dropped".to_string(),
                ))
            }
            Err(_) => {
                self.reply_server.shared.router.remove(&request_id).await;
                Err(AdapterError::Timeout)
            }
        }
    }

    fn kind(&self) -> &'static str {
        "openclaw-channel"
    }
}

async fn send_inbound_with_retries(
    client: &reqwest::Client,
    url: &str,
    auth_token: &str,
    body: &InboundPayload<'_>,
) -> Result<reqwest::Response, AdapterError> {
    let mut attempt = 0usize;

    loop {
        let mut req = client.post(url).json(body);
        if !auth_token.is_empty() {
            req = req.bearer_auth(auth_token);
        }

        match req.send().await {
            Ok(resp) => return Ok(resp),
            Err(e) if e.is_timeout() => return Err(AdapterError::Timeout),
            Err(e)
                if should_retry_inbound_connect_error(&e)
                    && attempt < INBOUND_CONNECT_RETRY_DELAYS_MS.len() =>
            {
                let delay_ms = INBOUND_CONNECT_RETRY_DELAYS_MS[attempt];
                attempt += 1;
                warn!(
                    endpoint = %url,
                    attempt,
                    retry_delay_ms = delay_ms,
                    error = %format_reqwest_error(&e),
                    "openclaw-channel inbound connect failed; retrying"
                );
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            Err(e) => return Err(AdapterError::Unavailable(format_reqwest_error(&e))),
        }
    }
}

fn should_retry_inbound_connect_error(error: &reqwest::Error) -> bool {
    error.is_connect()
}

fn format_reqwest_error(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut source = StdError::source(error);
    while let Some(err) = source {
        message.push_str(": ");
        message.push_str(&err.to_string());
        source = err.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use serde_json::Value;
    use tokio::sync::Mutex as TokioMutex;

    use crate::sync::Arc;

    #[derive(Clone)]
    struct CaptureState {
        last_body: Arc<TokioMutex<Option<Value>>>,
        reply_webhook: Option<String>,
        reply_auth: Option<String>,
        reply_attachments: Option<Value>,
    }

    async fn inbound_handler(
        State(state): State<CaptureState>,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        *state.last_body.lock().await = Some(body.clone());

        if let Some(webhook) = state.reply_webhook {
            let session_key = body
                .get("sessionKey")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let request_id = body
                .get("requestId")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();

            let mut reply = serde_json::json!({
                "sessionKey": session_key,
                "requestId": request_id,
                "message": "reply from openclaw",
                "channel": "whatsapp",
                "to": "+15555550001"
            });
            if let Some(attachments) = state.reply_attachments {
                reply["attachments"] = attachments;
            }

            let mut req = reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("test reqwest client")
                .post(webhook)
                .json(&reply);

            if let Some(token) = state.reply_auth {
                req = req.bearer_auth(token);
            }

            tokio::spawn(async move {
                let _ = req.send().await;
            });
        }

        (StatusCode::OK, Json(serde_json::json!({ "ok": true })))
    }

    async fn start_inbound_server(state: CaptureState) -> u16 {
        let app = Router::new()
            .route("/calciforge/inbound", post(inbound_handler))
            .with_state(state);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        port
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn make_adapter(
        endpoint: String,
        reply_port: u16,
        reply_auth_token: Option<String>,
    ) -> OpenClawChannelAdapter {
        OpenClawChannelAdapter::new(
            endpoint,
            "hooks-test-token".to_string(),
            "main".to_string(),
            Some(reply_port),
            reply_auth_token,
            Some(3000),
        )
    }

    static ENV_LOCK: TokioMutex<()> = TokioMutex::const_new(());

    struct EnvRestore {
        key: &'static str,
        value: Option<String>,
    }

    impl EnvRestore {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            // SAFETY: this test serializes its own environment mutations. The
            // bogus proxy is scoped by restore guards and the test suite keeps
            // local HTTP clients explicit about proxy behavior.
            unsafe {
                std::env::set_var(key, value);
            }
            Self {
                key,
                value: previous,
            }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            // SAFETY: see EnvRestore::set.
            unsafe {
                match &self.value {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    #[tokio::test]
    async fn test_dispatch_sends_expected_inbound_payload() {
        let captured = Arc::new(TokioMutex::new(None));
        let reply_port = free_port();

        let state = CaptureState {
            last_body: captured.clone(),
            reply_webhook: Some(format!("http://127.0.0.1:{reply_port}/hooks/reply")),
            reply_auth: Some("reply-secret".to_string()),
            reply_attachments: None,
        };
        let inbound_port = start_inbound_server(state).await;

        let adapter = make_adapter(
            format!("http://127.0.0.1:{inbound_port}"),
            reply_port,
            Some("reply-secret".to_string()),
        );

        let reply = adapter
            .dispatch_with_context(DispatchContext {
                message: "hello from calciforge",
                sender: Some("brian"),
                model_override: None,
                session: None,
                channel: Some("telegram"),
            })
            .await
            .expect("dispatch should succeed");

        assert_eq!(reply, "reply from openclaw");

        let body = captured
            .lock()
            .await
            .clone()
            .expect("expected inbound payload");

        assert_eq!(
            body.get("message").and_then(|v| v.as_str()),
            Some("hello from calciforge")
        );
        assert_eq!(
            body.get("sessionKey").and_then(|v| v.as_str()),
            Some("calciforge:main:brian")
        );
        let request_id = body
            .get("requestId")
            .and_then(|v| v.as_str())
            .expect("requestId should be present");
        assert_ne!(request_id, "calciforge:main:brian");
        uuid::Uuid::parse_str(request_id).expect("requestId should be a UUID");
        assert_eq!(body.get("sender").and_then(|v| v.as_str()), Some("brian"));
        assert_eq!(
            body.get("channel").and_then(|v| v.as_str()),
            Some("telegram")
        );
        assert_eq!(body.get("agentId").and_then(|v| v.as_str()), Some("main"));
    }

    #[tokio::test]
    async fn test_dispatch_returns_reply_from_hooks_reply() {
        let captured = Arc::new(TokioMutex::new(None));
        let reply_port = free_port();

        let state = CaptureState {
            last_body: captured,
            reply_webhook: Some(format!("http://127.0.0.1:{reply_port}/hooks/reply")),
            reply_auth: None,
            reply_attachments: None,
        };
        let inbound_port = start_inbound_server(state).await;

        let adapter = make_adapter(format!("http://127.0.0.1:{inbound_port}"), reply_port, None);

        let reply = adapter
            .dispatch_with_context(DispatchContext {
                message: "route this",
                sender: Some("renee"),
                model_override: None,
                session: None,
                channel: None,
            })
            .await
            .expect("dispatch should return reply callback");

        assert_eq!(reply, "reply from openclaw");
    }

    #[tokio::test]
    async fn test_dispatch_ignores_ambient_proxy_for_agent_control_plane() {
        let _env_lock = ENV_LOCK.lock().await;
        let _http_proxy = EnvRestore::set("HTTP_PROXY", "http://127.0.0.1:9");
        let _http_proxy_lower = EnvRestore::set("http_proxy", "http://127.0.0.1:9");
        let _no_proxy = EnvRestore::set("NO_PROXY", "");
        let _no_proxy_lower = EnvRestore::set("no_proxy", "");

        let captured = Arc::new(TokioMutex::new(None));
        let reply_port = free_port();

        let state = CaptureState {
            last_body: captured,
            reply_webhook: Some(format!("http://127.0.0.1:{reply_port}/hooks/reply")),
            reply_auth: None,
            reply_attachments: None,
        };
        let app = Router::new()
            .route("/calciforge/inbound", post(inbound_handler))
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let inbound_port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let adapter = make_adapter(format!("http://127.0.0.1:{inbound_port}"), reply_port, None);
        let reply = adapter
            .dispatch_with_context(DispatchContext {
                message: "route this without ambient proxy",
                sender: Some("renee"),
                model_override: None,
                session: None,
                channel: None,
            })
            .await
            .expect("agent control-plane HTTP must bypass ambient proxy settings");

        assert_eq!(reply, "reply from openclaw");
    }

    #[tokio::test]
    async fn test_dispatch_retries_transient_inbound_connect_failure() {
        let captured = Arc::new(TokioMutex::new(None));
        let reply_port = free_port();
        let inbound_port = free_port();

        let state = CaptureState {
            last_body: captured,
            reply_webhook: Some(format!("http://127.0.0.1:{reply_port}/hooks/reply")),
            reply_auth: None,
            reply_attachments: None,
        };
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let app = Router::new()
                .route("/calciforge/inbound", post(inbound_handler))
                .with_state(state);
            let listener = TcpListener::bind(("127.0.0.1", inbound_port))
                .await
                .expect("delayed inbound test server should bind");
            let _ = axum::serve(listener, app).await;
        });

        let adapter = make_adapter(format!("http://127.0.0.1:{inbound_port}"), reply_port, None);
        let reply = adapter
            .dispatch_with_context(DispatchContext {
                message: "survive one refused connection",
                sender: Some("brian"),
                model_override: None,
                session: None,
                channel: Some("telegram"),
            })
            .await
            .expect("dispatch should retry a transient inbound connect failure");

        assert_eq!(reply, "reply from openclaw");
    }

    #[tokio::test]
    async fn test_dispatch_accepts_legacy_session_key_only_callback() {
        #[derive(Clone)]
        struct LegacyState {
            reply_webhook: String,
        }

        async fn legacy_inbound_handler(
            State(state): State<LegacyState>,
            Json(body): Json<Value>,
        ) -> (StatusCode, Json<serde_json::Value>) {
            let session_key = body
                .get("sessionKey")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let reply = serde_json::json!({
                "sessionKey": session_key,
                "message": "legacy reply",
            });

            tokio::spawn(async move {
                let _ = reqwest::Client::builder()
                    .no_proxy()
                    .build()
                    .expect("test reqwest client")
                    .post(state.reply_webhook)
                    .json(&reply)
                    .send()
                    .await;
            });

            (StatusCode::OK, Json(serde_json::json!({ "ok": true })))
        }

        let reply_port = free_port();
        let app = Router::new()
            .route("/calciforge/inbound", post(legacy_inbound_handler))
            .with_state(LegacyState {
                reply_webhook: format!("http://127.0.0.1:{reply_port}/hooks/reply"),
            });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let inbound_port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let adapter = make_adapter(format!("http://127.0.0.1:{inbound_port}"), reply_port, None);
        let reply = adapter
            .dispatch_with_context(DispatchContext {
                message: "route this",
                sender: Some("renee"),
                model_override: None,
                session: None,
                channel: None,
            })
            .await
            .expect("legacy callback should still route by sessionKey");

        assert_eq!(reply, "legacy reply");
    }

    #[tokio::test]
    async fn test_dispatch_treats_empty_request_id_as_legacy_callback() {
        #[derive(Clone)]
        struct LegacyState {
            reply_webhook: String,
        }

        async fn legacy_inbound_handler(
            State(state): State<LegacyState>,
            Json(body): Json<Value>,
        ) -> (StatusCode, Json<serde_json::Value>) {
            let session_key = body
                .get("sessionKey")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let reply = serde_json::json!({
                "sessionKey": session_key,
                "requestId": "",
                "message": "empty request id legacy reply",
            });

            tokio::spawn(async move {
                let _ = reqwest::Client::builder()
                    .no_proxy()
                    .build()
                    .expect("test reqwest client")
                    .post(state.reply_webhook)
                    .json(&reply)
                    .send()
                    .await;
            });

            (StatusCode::OK, Json(serde_json::json!({ "ok": true })))
        }

        let reply_port = free_port();
        let app = Router::new()
            .route("/calciforge/inbound", post(legacy_inbound_handler))
            .with_state(LegacyState {
                reply_webhook: format!("http://127.0.0.1:{reply_port}/hooks/reply"),
            });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let inbound_port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let adapter = make_adapter(format!("http://127.0.0.1:{inbound_port}"), reply_port, None);
        let reply = adapter
            .dispatch_with_context(DispatchContext {
                message: "route this with empty request id fallback",
                sender: Some("renee"),
                model_override: None,
                session: None,
                channel: None,
            })
            .await
            .expect("empty requestId should fall back to sessionKey");

        assert_eq!(reply, "empty request id legacy reply");
    }

    #[tokio::test]
    async fn test_legacy_session_key_callback_is_ambiguous_for_overlapping_dispatches() {
        let router = ReplyRouter::new();
        let (first_tx, first_rx) = oneshot::channel::<ReplyResult>();
        let (second_tx, second_rx) = oneshot::channel::<ReplyResult>();
        let session_key = "calciforge:main:brian".to_string();

        router
            .insert("request-1".to_string(), session_key.clone(), first_tx)
            .await;
        router
            .insert("request-2".to_string(), session_key.clone(), second_tx)
            .await;

        assert!(
            router.take(&session_key).await.is_none(),
            "legacy sessionKey-only callback must fail closed once the session has overlapping requests"
        );

        let first = router
            .take("request-1")
            .await
            .expect("requestId correlation for first request should remain available");
        first
            .send(Ok(OutboundMessage::text("first")))
            .expect("first receiver should still be live");
        assert_eq!(
            first_rx.await.unwrap().unwrap().render_text_fallback(),
            "first"
        );

        let second = router
            .take("request-2")
            .await
            .expect("requestId correlation for second request should remain available");
        second
            .send(Ok(OutboundMessage::text("second")))
            .expect("second receiver should still be live");
        assert_eq!(
            second_rx.await.unwrap().unwrap().render_text_fallback(),
            "second"
        );
    }

    #[tokio::test]
    async fn test_overlapping_dispatches_same_session_are_correlated_by_request_id() {
        #[derive(Clone)]
        struct OverlapState {
            reply_webhook: String,
        }

        async fn overlap_inbound_handler(
            State(state): State<OverlapState>,
            Json(body): Json<Value>,
        ) -> (StatusCode, Json<serde_json::Value>) {
            let session_key = body
                .get("sessionKey")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let request_id = body
                .get("requestId")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let message = body
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();

            let webhook = state.reply_webhook.clone();
            tokio::spawn(async move {
                if message == "first" {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }

                let reply = serde_json::json!({
                    "sessionKey": session_key,
                    "requestId": request_id,
                    "message": format!("reply to {message}"),
                });
                let _ = reqwest::Client::builder()
                    .no_proxy()
                    .build()
                    .expect("test reqwest client")
                    .post(webhook)
                    .json(&reply)
                    .send()
                    .await;
            });

            (StatusCode::OK, Json(serde_json::json!({ "ok": true })))
        }

        let reply_port = free_port();
        let overlap_state = OverlapState {
            reply_webhook: format!("http://127.0.0.1:{reply_port}/hooks/reply"),
        };
        let app = Router::new()
            .route("/calciforge/inbound", post(overlap_inbound_handler))
            .with_state(overlap_state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let inbound_port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let adapter = Arc::new(make_adapter(
            format!("http://127.0.0.1:{inbound_port}"),
            reply_port,
            None,
        ));
        let first = {
            let adapter = adapter.clone();
            tokio::spawn(async move {
                adapter
                    .dispatch_with_context(DispatchContext {
                        message: "first",
                        sender: Some("brian"),
                        model_override: None,
                        session: None,
                        channel: None,
                    })
                    .await
            })
        };
        let second = {
            let adapter = adapter.clone();
            tokio::spawn(async move {
                adapter
                    .dispatch_with_context(DispatchContext {
                        message: "second",
                        sender: Some("brian"),
                        model_override: None,
                        session: None,
                        channel: None,
                    })
                    .await
            })
        };

        let (first_reply, second_reply) = tokio::join!(first, second);
        assert_eq!(
            first_reply
                .expect("first task should not panic")
                .expect("first dispatch should receive its own reply"),
            "reply to first"
        );
        assert_eq!(
            second_reply
                .expect("second task should not panic")
                .expect("second dispatch should receive its own reply"),
            "reply to second"
        );
    }

    #[tokio::test]
    async fn test_rebuilt_adapters_share_reply_server() {
        let captured = Arc::new(TokioMutex::new(None));
        let reply_port = free_port();

        let state = CaptureState {
            last_body: captured,
            reply_webhook: Some(format!("http://127.0.0.1:{reply_port}/hooks/reply")),
            reply_auth: Some("reply-secret".to_string()),
            reply_attachments: None,
        };
        let inbound_port = start_inbound_server(state).await;
        let endpoint = format!("http://127.0.0.1:{inbound_port}");

        let first = make_adapter(
            endpoint.clone(),
            reply_port,
            Some("reply-secret".to_string()),
        );
        let first_reply = first
            .dispatch_with_context(DispatchContext {
                message: "first",
                sender: Some("brian"),
                model_override: None,
                session: None,
                channel: None,
            })
            .await
            .expect("first dispatch should start reply server");
        assert_eq!(first_reply, "reply from openclaw");

        let second = make_adapter(endpoint, reply_port, Some("reply-secret".to_string()));
        let second_reply = second
            .dispatch_with_context(DispatchContext {
                message: "second",
                sender: Some("renee"),
                model_override: None,
                session: None,
                channel: None,
            })
            .await
            .expect("rebuilt adapter should reuse reply server/router");
        assert_eq!(second_reply, "reply from openclaw");
    }

    #[tokio::test]
    async fn test_reply_port_auth_mismatch_fails_before_dispatch() {
        let reply_port = free_port();
        let first = make_adapter(
            "http://127.0.0.1:1".to_string(),
            reply_port,
            Some("reply-secret".to_string()),
        );
        let _ = first.ensure_reply_server_started().await;

        let second = make_adapter(
            "http://127.0.0.1:1".to_string(),
            reply_port,
            Some("different-secret".to_string()),
        );
        let err = second
            .dispatch_with_context(DispatchContext {
                message: "will not send",
                sender: Some("brian"),
                model_override: None,
                session: None,
                channel: None,
            })
            .await
            .expect_err("conflicting reply auth token should fail");

        assert!(err
            .to_string()
            .contains("already registered with a different reply_auth_token"));
    }

    #[tokio::test]
    async fn test_dispatch_preserves_callback_attachments() {
        let captured = Arc::new(TokioMutex::new(None));
        let reply_port = free_port();

        let state = CaptureState {
            last_body: captured,
            reply_webhook: Some(format!("http://127.0.0.1:{reply_port}/hooks/reply")),
            reply_auth: None,
            reply_attachments: Some(serde_json::json!([
                {
                    "name": "../diagram.png",
                    "mimeType": "image/png",
                    "caption": "Generated diagram",
                    "dataBase64": "iVBORw0KGgo="
                }
            ])),
        };
        let inbound_port = start_inbound_server(state).await;

        let adapter = make_adapter(format!("http://127.0.0.1:{inbound_port}"), reply_port, None);

        let reply = adapter
            .dispatch_message_with_context(DispatchContext {
                message: "make a diagram",
                sender: Some("brian"),
                model_override: None,
                session: None,
                channel: None,
            })
            .await
            .expect("dispatch should preserve callback attachment");

        assert_eq!(reply.text.as_deref(), Some("reply from openclaw"));
        assert_eq!(reply.attachments.len(), 1);
        let attachment = &reply.attachments[0];
        assert_eq!(attachment.kind, crate::messages::AttachmentKind::Image);
        assert_eq!(attachment.mime_type, "image/png");
        assert_eq!(attachment.caption.as_deref(), Some("Generated diagram"));
        assert_eq!(attachment.size_bytes, 8);
        assert_eq!(
            attachment.path.file_name().and_then(|name| name.to_str()),
            Some("diagram.png")
        );
        assert!(
            attachment
                .path
                .starts_with(std::env::temp_dir().join("calciforge-openclaw-artifacts")),
            "attachment should be copied into Calciforge-owned storage: {:?}",
            attachment.path
        );
        assert!(std::fs::metadata(&attachment.path).unwrap().is_file());

        let fallback = reply.render_text_fallback();
        assert!(fallback.contains("diagram.png"));
        assert!(
            !fallback.contains("calciforge-openclaw-artifacts"),
            "fallback must not expose local artifact storage path: {fallback}"
        );
    }

    #[tokio::test]
    async fn test_malformed_callback_attachment_fails_dispatch() {
        let captured = Arc::new(TokioMutex::new(None));
        let reply_port = free_port();

        let state = CaptureState {
            last_body: captured,
            reply_webhook: Some(format!("http://127.0.0.1:{reply_port}/hooks/reply")),
            reply_auth: None,
            reply_attachments: Some(serde_json::json!([
                {
                    "name": "diagram.png",
                    "mimeType": "image/png"
                }
            ])),
        };
        let inbound_port = start_inbound_server(state).await;

        let adapter = make_adapter(format!("http://127.0.0.1:{inbound_port}"), reply_port, None);

        let err = adapter
            .dispatch_message_with_context(DispatchContext {
                message: "make a diagram",
                sender: Some("brian"),
                model_override: None,
                session: None,
                channel: None,
            })
            .await
            .expect_err("missing dataBase64 should fail the waiting dispatch");

        assert!(
            err.to_string().contains("missing dataBase64"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_duplicate_callback_attachment_names_are_disambiguated() {
        let captured = Arc::new(TokioMutex::new(None));
        let reply_port = free_port();

        let state = CaptureState {
            last_body: captured,
            reply_webhook: Some(format!("http://127.0.0.1:{reply_port}/hooks/reply")),
            reply_auth: None,
            reply_attachments: Some(serde_json::json!([
                {
                    "name": "diagram.png",
                    "mimeType": "image/png",
                    "dataBase64": "iVBORw0KGgo="
                },
                {
                    "name": "diagram.png",
                    "mimeType": "image/png",
                    "dataBase64": "iVBORw0KGgo="
                }
            ])),
        };
        let inbound_port = start_inbound_server(state).await;

        let adapter = make_adapter(format!("http://127.0.0.1:{inbound_port}"), reply_port, None);

        let reply = adapter
            .dispatch_message_with_context(DispatchContext {
                message: "make two diagrams",
                sender: Some("brian"),
                model_override: None,
                session: None,
                channel: None,
            })
            .await
            .expect("duplicate names should be disambiguated");

        assert_eq!(reply.attachments.len(), 2);
        assert_ne!(reply.attachments[0].path, reply.attachments[1].path);
        assert_eq!(
            reply.attachments[0]
                .path
                .file_name()
                .and_then(|name| name.to_str()),
            Some("diagram.png")
        );
        assert_eq!(
            reply.attachments[1]
                .path
                .file_name()
                .and_then(|name| name.to_str()),
            Some("attachment-2-diagram.png")
        );
    }

    #[tokio::test]
    async fn test_reply_server_startup_error_is_reused_without_hanging() {
        let listener = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let reply_port = listener.local_addr().unwrap().port();
        let first = make_adapter("http://127.0.0.1:1".to_string(), reply_port, None);

        let first_err = first
            .ensure_reply_server_started()
            .await
            .expect_err("occupied reply port should fail startup");
        assert!(first_err.to_string().contains("failed to start"));

        let second = make_adapter("http://127.0.0.1:1".to_string(), reply_port, None);
        let second_result = tokio::time::timeout(
            Duration::from_millis(200),
            second.ensure_reply_server_started(),
        )
        .await
        .expect("stored startup failure should not wait forever");
        let second_err = second_result.expect_err("same startup error should be reused");
        assert!(second_err.to_string().contains("failed to start"));
    }
}
