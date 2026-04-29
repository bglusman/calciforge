//! OpenClawChannelAdapter — bridge Calciforge to the OpenClaw calciforge plugin.
//!
//! This adapter posts inbound messages to OpenClaw at
//! `POST /calciforge/inbound` (gateway auth) and waits for a correlated callback on the
//! local reply webhook `POST /hooks/reply`.

use std::collections::HashMap;
use std::time::Duration;

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

use super::{AdapterError, AgentAdapter, DispatchContext};

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const DEFAULT_REPLY_PORT: u16 = 18_797;

/// Correlates `sessionKey` callbacks to pending dispatch requests.
#[derive(Clone, Default)]
pub struct ReplyRouter {
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<String>>>>,
}

impl ReplyRouter {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn insert(&self, session_key: String, tx: oneshot::Sender<String>) {
        self.pending.lock().await.insert(session_key, tx);
    }

    pub async fn take(&self, session_key: &str) -> Option<oneshot::Sender<String>> {
        self.pending.lock().await.remove(session_key)
    }

    pub async fn remove(&self, session_key: &str) {
        self.pending.lock().await.remove(session_key);
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
    message: String,
    #[allow(dead_code)]
    channel: Option<String>,
    #[allow(dead_code)]
    to: Option<String>,
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

    if let Some(tx) = state.router.take(&payload.session_key).await {
        let _ = tx.send(payload.message);
        (StatusCode::OK, Json(AckResponse { ok: true }))
    } else {
        warn!(session_key = %payload.session_key, "openclaw-channel reply without pending request");
        (StatusCode::ACCEPTED, Json(AckResponse { ok: true }))
    }
}

#[derive(Debug, Serialize)]
struct InboundPayload<'a> {
    message: &'a str,
    #[serde(rename = "sessionKey")]
    session_key: String,
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
        self.ensure_reply_server_started().await?;

        let sender = ctx.sender.unwrap_or("unknown");
        let session_key = self.session_key_for(sender);
        let (tx, rx) = oneshot::channel::<String>();
        self.reply_server
            .shared
            .router
            .insert(session_key.clone(), tx)
            .await;

        let body = InboundPayload {
            message: ctx.message,
            session_key: session_key.clone(),
            sender,
            // DispatchContext does not currently carry channel-specific routing metadata.
            channel: None,
            reply_to: None,
            agent_id: &self.openclaw_agent_id,
        };

        let url = self.inbound_url();
        debug!(endpoint = %url, sender, session_key = %session_key, "openclaw-channel dispatch");

        let mut req = self.client.post(&url).json(&body);
        if !self.auth_token.is_empty() {
            req = req.bearer_auth(&self.auth_token);
        }

        let inbound_resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                AdapterError::Timeout
            } else {
                AdapterError::Unavailable(e.to_string())
            }
        });

        let inbound_resp = match inbound_resp {
            Ok(r) => r,
            Err(e) => {
                self.reply_server.shared.router.remove(&session_key).await;
                return Err(e);
            }
        };

        if !inbound_resp.status().is_success() {
            let status = inbound_resp.status();
            let body = inbound_resp.text().await.unwrap_or_default();
            self.reply_server.shared.router.remove(&session_key).await;
            return Err(AdapterError::Protocol(format!(
                "openclaw-channel inbound HTTP {}: {}",
                status, body
            )));
        }

        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => {
                self.reply_server.shared.router.remove(&session_key).await;
                Err(AdapterError::Protocol(
                    "openclaw-channel reply receiver dropped".to_string(),
                ))
            }
            Err(_) => {
                self.reply_server.shared.router.remove(&session_key).await;
                Err(AdapterError::Timeout)
            }
        }
    }

    fn kind(&self) -> &'static str {
        "openclaw-channel"
    }
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

            let mut req = reqwest::Client::new()
                .post(webhook)
                .json(&serde_json::json!({
                    "sessionKey": session_key,
                    "message": "reply from openclaw",
                    "channel": "whatsapp",
                    "to": "+15555550001"
                }));

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

    #[tokio::test]
    async fn test_dispatch_sends_expected_inbound_payload() {
        let captured = Arc::new(TokioMutex::new(None));
        let reply_port = free_port();

        let state = CaptureState {
            last_body: captured.clone(),
            reply_webhook: Some(format!("http://127.0.0.1:{reply_port}/hooks/reply")),
            reply_auth: Some("reply-secret".to_string()),
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
        assert_eq!(body.get("sender").and_then(|v| v.as_str()), Some("brian"));
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
        };
        let inbound_port = start_inbound_server(state).await;

        let adapter = make_adapter(format!("http://127.0.0.1:{inbound_port}"), reply_port, None);

        let reply = adapter
            .dispatch_with_context(DispatchContext {
                message: "route this",
                sender: Some("renee"),
                model_override: None,
            })
            .await
            .expect("dispatch should return reply callback");

        assert_eq!(reply, "reply from openclaw");
    }

    #[tokio::test]
    async fn test_rebuilt_adapters_share_reply_server() {
        let captured = Arc::new(TokioMutex::new(None));
        let reply_port = free_port();

        let state = CaptureState {
            last_body: captured,
            reply_webhook: Some(format!("http://127.0.0.1:{reply_port}/hooks/reply")),
            reply_auth: Some("reply-secret".to_string()),
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
            })
            .await
            .expect_err("conflicting reply auth token should fail");

        assert!(err
            .to_string()
            .contains("already registered with a different reply_auth_token"));
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
