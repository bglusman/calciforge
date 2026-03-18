//! Signal channel adapter for PolyClaw v2.
//!
//! ## Architecture
//!
//! This channel is a **webhook receiver + identity router + reply sender**.
//!
//! The actual Signal protocol handling (registration, encryption, send/receive
//! at the Signal wire level) lives in the OpenClaw `message` tool with
//! `action = "send"` and `channel = "signal"`. PolyClaw acts as a routing
//! sidecar:
//!
//! ```text
//! Signal user  →  OpenClaw (signal-web)  →  POST /webhooks/signal  →  PolyClaw
//!                                                                          │
//!                                              identity resolution         │
//!                                              agent dispatch              │
//!                                                                          ↓
//! Signal user  ←  OpenClaw (signal-web)  ←  POST /tools/invoke  ←  PolyClaw reply
//! ```
//!
//! ## Webhook payload format
//!
//! Incoming messages are expected in the Signal REST API webhook format
//! (as produced by the OpenClaw signal-web receive path):
//!
//! ```json
//! {
//!   "account": "+15555550001",
//!   "envelope": {
//!     "source": "+14155551234",
//!     "sourceNumber": "+14155551234",
//!     "sourceName": "Alice",
//!     "sourceDevice": 1,
//!     "timestamp": 1699999999999,
//!     "dataMessage": {
//!       "message": "Hello from Signal!",
//!       "timestamp": 1699999999999
//!     }
//!   }
//! }
//! ```
//!
//! The `sourceNumber` field is the sender's E.164 phone number.
//! PolyClaw uses it directly for identity lookup.
//!
//! ## Config
//!
//! ```toml
//! [[channels]]
//! kind = "signal"
//! enabled = true
//! # OpenClaw gateway endpoint — the running OpenClaw instance that owns the Signal session
//! # and can forward messages via its /tools/invoke HTTP API.
//! nzc_endpoint = "http://127.0.0.1:18789"
//! # Bearer token for the OpenClaw gateway
//! nzc_auth_token = "REPLACE_WITH_AUTH_TOKEN"
//! # Webhook path PolyClaw registers (on the PolyClaw gateway HTTP server)
//! webhook_path = "/webhooks/signal"
//! # HMAC secret for webhook signature verification (optional but recommended)
//! webhook_secret = "your-shared-secret"
//! # Webhook HTTP listen address (host:port). Default: 0.0.0.0:18796
//! webhook_listen = "0.0.0.0:18796"
//! # Allowed E.164 phone numbers. Must match identity aliases with channel = "signal".
//! allowed_numbers = ["+14155551234"]
//! ```

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

use crate::{
    auth::{find_agent, resolve_channel_sender},
    commands::CommandHandler,
    config::PolyConfig,
    context::ContextStore,
    router::Router,
};

// ---------------------------------------------------------------------------
// Incoming webhook payload types (Signal REST API format)
// ---------------------------------------------------------------------------

/// Top-level Signal REST API webhook body.
#[derive(Debug, Deserialize)]
struct SignalWebhookPayload {
    /// The receiving account (our Signal number).
    #[serde(default)]
    account: Option<String>,
    /// The message envelope containing sender and message data.
    #[serde(default)]
    envelope: Option<SignalEnvelope>,
}

#[derive(Debug, Deserialize)]
struct SignalEnvelope {
    /// Sender phone number in E.164 format.
    #[serde(default)]
    source: Option<String>,
    /// Alternative field for sender number (some Signal API versions).
    #[serde(rename = "sourceNumber", default)]
    source_number: Option<String>,
    /// Display name (if known in sender's contacts - unused for identity).
    #[serde(rename = "sourceName", default)]
    _source_name: Option<String>,
    /// Device ID of the sender.
    #[serde(rename = "sourceDevice", default)]
    _source_device: Option<u32>,
    /// Unix timestamp in milliseconds.
    #[serde(default)]
    timestamp: Option<u64>,
    /// The actual message data.
    #[serde(rename = "dataMessage", default)]
    data_message: Option<SignalDataMessage>,
}

#[derive(Debug, Deserialize)]
struct SignalDataMessage {
    /// The message text content.
    #[serde(default)]
    message: Option<String>,
    /// Timestamp (redundant with envelope timestamp).
    #[serde(default)]
    timestamp: Option<u64>,
}

/// A parsed, normalised inbound Signal message.
#[derive(Debug, Clone)]
pub struct InboundSignalMessage {
    /// Sender phone number in E.164 format (e.g. `"+14155551234"`).
    pub from: String,
    /// Message text content.
    pub text: String,
    /// Unix timestamp in seconds.
    pub timestamp: u64,
}

// ---------------------------------------------------------------------------
// Outbound reply request (for the NZC /tools/invoke API)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ToolInvokeRequest {
    tool: &'static str,
    args: ToolInvokeArgs,
}

#[derive(Debug, Serialize)]
struct ToolInvokeArgs {
    action: &'static str,
    channel: &'static str,
    target: String,
    message: String,
}

// ---------------------------------------------------------------------------
// Signal channel
// ---------------------------------------------------------------------------

/// Signal channel adapter.
///
/// Runs an HTTP server listening for incoming webhook POSTs from OpenClaw
/// (or any conforming Signal webhook source), resolves sender identity,
/// dispatches to the configured agent, and sends the reply back via the
/// OpenClaw `/tools/invoke` API.
pub struct SignalChannel {
    config: Arc<PolyConfig>,
    router: Arc<Router>,
    command_handler: Arc<CommandHandler>,
    context_store: ContextStore,
    http_client: reqwest::Client,
}

impl SignalChannel {
    pub fn new(
        config: Arc<PolyConfig>,
        router: Arc<Router>,
        command_handler: Arc<CommandHandler>,
        context_store: ContextStore,
    ) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("failed to build HTTP client");

        Self {
            config,
            router,
            command_handler,
            context_store,
            http_client,
        }
    }

    /// Parse an incoming webhook payload and return all valid inbound messages.
    ///
    /// Filters out:
    /// - Non-text messages (receipts, typing indicators, etc.)
    /// - Messages from numbers not in the `allowed_numbers` list
    /// - Messages with empty body
    pub fn parse_webhook_payload(
        &self,
        raw: &serde_json::Value,
        allowed_numbers: &[String],
    ) -> Vec<InboundSignalMessage> {
        let mut messages = Vec::new();

        let payload: SignalWebhookPayload = match serde_json::from_value(raw.clone()) {
            Ok(p) => p,
            Err(e) => {
                warn!("Signal: failed to deserialise webhook payload: {e}");
                return messages;
            }
        };

        let Some(envelope) = payload.envelope else {
            debug!("Signal: webhook has no envelope, skipping");
            return messages;
        };

        // Get sender number from source or source_number field
        let from = envelope
            .source
            .or(envelope.source_number)
            .unwrap_or_default();

        if from.is_empty() {
            warn!("Signal: message has no source number, skipping");
            return messages;
        }

        // Normalise phone number to E.164 (add + if missing)
        let from = normalise_phone(&from);

        // Allowlist check
        if !is_number_allowed(&from, allowed_numbers) {
            warn!(
                from = %from,
                "Signal: dropping message from number not in allowed_numbers"
            );
            return messages;
        }

        // Get message text from dataMessage
        let Some(data_msg) = envelope.data_message else {
            debug!("Signal: envelope has no dataMessage, skipping");
            return messages;
        };

        let text = match data_msg.message {
            Some(t) if !t.trim().is_empty() => t,
            _ => {
                debug!("Signal: skipping empty text message");
                return messages;
            }
        };

        // Extract timestamp (prefer dataMessage timestamp, fall back to envelope)
        let timestamp_ms = data_msg.timestamp.or(envelope.timestamp).unwrap_or(0);
        let timestamp = timestamp_ms / 1000; // Convert ms to seconds

        messages.push(InboundSignalMessage {
            from,
            text,
            timestamp,
        });

        messages
    }

    /// Send a reply to a Signal user via the OpenClaw gateway `/tools/invoke` API.
    ///
    /// OpenClaw must be running with a live Signal session for this to succeed.
    /// Returns Ok(()) if the HTTP call was accepted (2xx), Err otherwise.
    pub async fn send_reply(
        &self,
        nzc_endpoint: &str,
        nzc_auth_token: Option<&str>,
        to: &str,
        text: &str,
    ) -> Result<()> {
        let url = format!("{nzc_endpoint}/tools/invoke");

        let body = ToolInvokeRequest {
            tool: "message",
            args: ToolInvokeArgs {
                action: "send",
                channel: "signal",
                target: to.to_string(),
                message: text.to_string(),
            },
        };

        let mut req = self.http_client.post(&url).json(&body);

        if let Some(token) = nzc_auth_token {
            req = req.bearer_auth(token);
        }

        let resp = req.send().await.with_context(|| {
            format!("Signal: HTTP error sending reply via {url}")
        })?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Signal: OpenClaw replied {status} for send to {to}: {body_text}"
            );
        }

        debug!(to = %to, "Signal: reply sent via OpenClaw");
        Ok(())
    }

    /// Handle a single inbound Signal message end-to-end.
    ///
    /// Performs identity lookup, command dispatch, agent routing, and reply.
    pub async fn handle_message(
        self: Arc<Self>,
        msg: InboundSignalMessage,
        nzc_endpoint: String,
        nzc_auth_token: Option<String>,
    ) {
        // Clone owned strings up front so they can be moved into spawned tasks.
        let from: String = msg.from.clone();
        let text: String = msg.text.clone();

        // Auth boundary: resolve sender to identity
        let identity = match resolve_channel_sender("signal", &from, &self.config) {
            Some(id) => id,
            None => {
                warn!(from = %from, "Signal: unknown sender — dropping");
                return;
            }
        };

        info!(
            identity = %identity.id,
            from = %from,
            text_len = %text.len(),
            "Signal: authorised message from identity"
        );

        // Context key: scoped per identity (phone is the key)
        let chat_key = format!("signal-{}", identity.id);

        // ── Command fast-path ──────────────────────────────────────────────

        // Pre-auth commands (!ping, !help, !agents, !metrics)
        if let Some(reply) = self.command_handler.handle(&text) {
            debug!(identity = %identity.id, cmd = %text.trim(), "Signal: handled pre-auth command");
            let channel = self.clone();
            let from_owned = from.clone();
            tokio::spawn(async move {
                if let Err(e) = channel
                    .send_reply(&nzc_endpoint, nzc_auth_token.as_deref(), &from_owned, &reply)
                    .await
                {
                    warn!(from = %from_owned, error = %e, "Signal: failed to send command reply");
                }
            });
            return;
        }

        // !status — post-auth command
        if CommandHandler::is_status_command(&text) {
            let reply = self.command_handler.cmd_status_for_identity(&identity.id).await;
            let channel = self.clone();
            let from_owned = from.clone();
            tokio::spawn(async move {
                if let Err(e) = channel
                    .send_reply(&nzc_endpoint, nzc_auth_token.as_deref(), &from_owned, &reply)
                    .await
                {
                    warn!(from = %from_owned, error = %e, "Signal: failed to send status reply");
                }
            });
            return;
        }

        // !switch — post-auth command
        if CommandHandler::is_switch_command(&text) {
            let reply = self.command_handler.handle_switch(&text, &identity.id);
            let channel = self.clone();
            let from_owned = from.clone();
            tokio::spawn(async move {
                if let Err(e) = channel
                    .send_reply(&nzc_endpoint, nzc_auth_token.as_deref(), &from_owned, &reply)
                    .await
                {
                    warn!(from = %from_owned, error = %e, "Signal: failed to send switch reply");
                }
            });
            return;
        }

        // !default — post-auth command
        if CommandHandler::is_default_command(&text) {
            let reply = self.command_handler.handle_default(&identity.id);
            let channel = self.clone();
            let from_owned = from.clone();
            tokio::spawn(async move {
                if let Err(e) = channel
                    .send_reply(&nzc_endpoint, nzc_auth_token.as_deref(), &from_owned, &reply)
                    .await
                {
                    warn!(from = %from_owned, error = %e, "Signal: failed to send default reply");
                }
            });
            return;
        }

        // !context clear
        if text.trim().eq_ignore_ascii_case("!context clear") {
            self.context_store.clear(&chat_key);
            let channel = self.clone();
            let from_owned = from.clone();
            tokio::spawn(async move {
                if let Err(e) = channel
                    .send_reply(
                        &nzc_endpoint,
                        nzc_auth_token.as_deref(),
                        &from_owned,
                        "🧹 Conversation context cleared.",
                    )
                    .await
                {
                    warn!(from = %from_owned, error = %e, "Signal: failed to send context-clear reply");
                }
            });
            return;
        }

        // ── Agent dispatch ─────────────────────────────────────────────────

        let agent_id = match self.command_handler.active_agent_for(&identity.id) {
            Some(id) => id,
            None => {
                warn!(identity = %identity.id, "Signal: no routing rule for identity — dropping");
                return;
            }
        };

        let agent = match find_agent(&agent_id, &self.config) {
            Some(a) => a.clone(),
            None => {
                warn!(agent_id = %agent_id, "Signal: agent not in config");
                let channel = self.clone();
                let from_owned = from.clone();
                tokio::spawn(async move {
                    let _ = channel
                        .send_reply(
                            &nzc_endpoint,
                            nzc_auth_token.as_deref(),
                            &from_owned,
                            "⚠️ Agent not configured.",
                        )
                        .await;
                });
                return;
            }
        };

        // Sender label for context preambles
        let sender_label = self
            .config
            .identities
            .iter()
            .find(|i| i.id == identity.id)
            .and_then(|i| i.display_name.as_deref())
            .unwrap_or(&identity.id)
            .to_string();

        let identity_id = identity.id.clone();

        // Spawn agent dispatch — handler returns immediately
        tokio::spawn(async move {
            let augmented = self
                .context_store
                .augment_message(&chat_key, &agent_id, &text);

            let dispatch_start = std::time::Instant::now();
            match self.router.dispatch_with_sender(&augmented, &agent, &self.config, Some(&identity_id)).await {
                Ok(response) => {
                    let latency_ms = dispatch_start.elapsed().as_millis() as u64;
                    self.command_handler.record_dispatch(latency_ms);

                    debug!(
                        identity = %identity_id,
                        agent_id = %agent_id,
                        response_len = %response.len(),
                        "Signal: got agent response"
                    );

                    // Record exchange in context buffer
                    self.context_store.push(
                        &chat_key,
                        &sender_label,
                        &text,
                        &agent_id,
                        &response,
                    );

                    if let Err(e) = self
                        .send_reply(
                            &nzc_endpoint,
                            nzc_auth_token.as_deref(),
                            &from,
                            &response,
                        )
                        .await
                    {
                        warn!(
                            identity = %identity_id,
                            error = %e,
                            "Signal: failed to send agent reply"
                        );
                    }
                }
                Err(e) => {
                    warn!(identity = %identity_id, error = %e, "Signal: agent dispatch failed");
                    let _ = self
                        .send_reply(
                            &nzc_endpoint,
                            nzc_auth_token.as_deref(),
                            &from,
                            &format!("⚠️ Agent error: {e}"),
                        )
                        .await;
                }
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Webhook HTTP server
// ---------------------------------------------------------------------------

/// Run the Signal webhook HTTP listener.
///
/// Starts an HTTP server on `listen_addr` that accepts POST requests on
/// `webhook_path`. For each valid inbound message, spawns a handler task that
/// routes through PolyClaw's identity/agent system and sends the reply via
/// OpenClaw's `/tools/invoke` API.
///
/// This function runs until the server errors or is cancelled.
pub async fn run(
    config: Arc<PolyConfig>,
    router: Arc<Router>,
    command_handler: Arc<CommandHandler>,
    context_store: ContextStore,
) -> Result<()> {
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Find Signal channel config
    let signal_channel_cfg = config
        .channels
        .iter()
        .find(|c| c.kind == "signal" && c.enabled)
        .context("no enabled signal channel found in config")?;

    let listen_addr: SocketAddr = signal_channel_cfg
        .webhook_listen
        .as_deref()
        .unwrap_or("0.0.0.0:18796")
        .parse()
        .context("invalid signal webhook_listen address")?;

    let nzc_endpoint = signal_channel_cfg
        .nzc_endpoint
        .as_deref()
        .unwrap_or("http://127.0.0.1:18789")
        .to_string();

    let nzc_auth_token = signal_channel_cfg.nzc_auth_token.clone();
    let webhook_path = signal_channel_cfg
        .webhook_path
        .as_deref()
        .unwrap_or("/webhooks/signal")
        .to_string();
    let webhook_secret = signal_channel_cfg.webhook_secret.clone();
    let allowed_numbers = signal_channel_cfg.allowed_numbers.clone();

    info!(
        listen = %listen_addr,
        path = %webhook_path,
        nzc = %nzc_endpoint,
        "Signal webhook channel starting"
    );

    let channel = Arc::new(SignalChannel::new(
        config,
        router,
        command_handler,
        context_store,
    ));

    let listener = tokio::net::TcpListener::bind(listen_addr)
        .await
        .with_context(|| format!("binding Signal webhook listener on {listen_addr}"))?;

    info!(addr = %listen_addr, "Signal webhook listener ready");

    loop {
        let (mut stream, peer_addr) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                warn!(error = %e, "Signal: accept error");
                continue;
            }
        };

        let channel = channel.clone();
        let nzc_endpoint = nzc_endpoint.clone();
        let nzc_auth_token = nzc_auth_token.clone();
        let webhook_path = webhook_path.clone();
        let webhook_secret = webhook_secret.clone();
        let allowed_numbers = allowed_numbers.clone();

        tokio::spawn(async move {
            // Read the raw HTTP request (max 256 KB)
            let mut buf = vec![0u8; 262_144];
            let n = match stream.read(&mut buf).await {
                Ok(n) => n,
                Err(e) => {
                    warn!(peer = %peer_addr, error = %e, "Signal: read error");
                    return;
                }
            };

            let raw = match std::str::from_utf8(&buf[..n]) {
                Ok(s) => s,
                Err(_) => {
                    let _ = send_http_response(&mut stream, 400, "Bad Request", "Invalid UTF-8").await;
                    return;
                }
            };

            // Parse method and path from first line
            let first_line = raw.lines().next().unwrap_or("");
            let mut parts = first_line.splitn(3, ' ');
            let method = parts.next().unwrap_or("").to_uppercase();
            let path = parts.next().unwrap_or("");

            // Health check
            if method == "GET" && (path == "/health" || path == "/healthz") {
                let _ = send_http_response(&mut stream, 200, "OK", r#"{"status":"ok"}"#).await;
                return;
            }

            // Only POST to the configured webhook path
            if method != "POST" || path != webhook_path {
                let _ = send_http_response(&mut stream, 404, "Not Found", r#"{"error":"not found"}"#).await;
                return;
            }

            // Extract body (everything after the blank line that separates headers from body)
            let body = if let Some(idx) = raw.find("\r\n\r\n") {
                &raw[idx + 4..]
            } else if let Some(idx) = raw.find("\n\n") {
                &raw[idx + 2..]
            } else {
                ""
            };

            // Optional HMAC verification
            if let Some(ref secret) = webhook_secret {
                let signature = raw
                    .lines()
                    .find(|l| l.to_lowercase().starts_with("x-hub-signature-256:"))
                    .and_then(|l| l.splitn(2, ':').nth(1))
                    .map(|s| s.trim());

                if let Some(sig) = signature {
                    if !verify_hmac_sha256(secret, body, sig) {
                        warn!(peer = %peer_addr, "Signal: HMAC verification failed");
                        let _ = send_http_response(&mut stream, 401, "Unauthorized", r#"{"error":"invalid signature"}"#).await;
                        return;
                    }
                } else {
                    warn!(peer = %peer_addr, "Signal: missing HMAC signature");
                    let _ = send_http_response(&mut stream, 401, "Unauthorized", r#"{"error":"missing signature"}"#).await;
                    return;
                }
            }

            // Parse JSON body
            let json: serde_json::Value = match serde_json::from_str(body) {
                Ok(v) => v,
                Err(e) => {
                    warn!(peer = %peer_addr, error = %e, "Signal: JSON parse error");
                    let _ = send_http_response(&mut stream, 400, "Bad Request", r#"{"error":"invalid json"}"#).await;
                    return;
                }
            };

            // Parse and handle messages
            let messages = channel.parse_webhook_payload(&json, &allowed_numbers);

            if messages.is_empty() {
                // Return 200 even if no messages (could be a valid webhook with no actionable content)
                let _ = send_http_response(&mut stream, 200, "OK", r#"{"received":true}"#).await;
                return;
            }

            // Acknowledge receipt immediately
            if let Err(e) = send_http_response(&mut stream, 200, "OK", r#"{"received":true}"#).await {
                warn!(peer = %peer_addr, error = %e, "Signal: failed to send ack");
            }

            // Handle each message asynchronously
            for msg in messages {
                let ch = channel.clone();
                let nzc_ep = nzc_endpoint.clone();
                let nzc_token = nzc_auth_token.clone();
                tokio::spawn(async move {
                    ch.handle_message(msg, nzc_ep, nzc_token).await;
                });
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Normalise a phone number to E.164 format (with leading +).
fn normalise_phone(num: &str) -> String {
    let trimmed = num.trim();
    if trimmed.starts_with('+') {
        trimmed.to_string()
    } else {
        format!("+{trimmed}")
    }
}

/// Check if a phone number is in the allowed list.
/// Supports wildcard "*" to allow any number (not recommended for production).
fn is_number_allowed(id: &str, allowed: &[String]) -> bool {
    if allowed.is_empty() {
        return false;
    }
    allowed.iter().any(|a| a == "*" || a == id)
}

/// Verify HMAC-SHA256 signature.
fn verify_hmac_sha256(secret: &str, body: &str, signature: &str) -> bool {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    type HmacSha256 = Hmac<Sha256>;

    let sig_bytes = match hex::decode(signature.strip_prefix("sha256=").unwrap_or(signature)) {
        Ok(b) => b,
        Err(_) => return false,
    };

    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };

    mac.update(body.as_bytes());
    mac.verify_slice(&sig_bytes).is_ok()
}

/// Send a simple HTTP response.
async fn send_http_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    status_text: &str,
    body: &str,
) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await
}
