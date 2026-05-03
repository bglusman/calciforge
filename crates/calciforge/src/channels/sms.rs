//! Text/iMessage channel adapter for Calciforge.
//!
//! Calciforge exposes `kind = "sms"` and uses `zeroclawlabs::LinqChannel`
//! underneath. Linq is webhook based for inbound iMessage/RCS/SMS events, so
//! this module hosts a small webhook receiver, lets the zeroclawlabs parser
//! normalize incoming payloads, then sends replies through the same `Channel`
//! interface used by other embedded transports.

use crate::sync::Arc;
use anyhow::{Context, Result};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router as AxumRouter,
};
use serde_json::json;
use tracing::{debug, info, warn};
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_channels::linq::LinqChannel as ZclLinqChannel;

use crate::{
    auth::{find_agent, resolve_channel_sender},
    choice_state::{ChoiceMatchResult, ChoiceState},
    commands::CommandHandler,
    config::{expand_tilde, CalciforgeConfig},
    context::ContextStore,
    messages::OutboundMessage,
    router::Router,
};

use super::telemetry;

use adversary_detector::middleware::ChannelScanner;
use adversary_detector::verdict::ScanContext;

pub struct SmsChannel<C: Channel + ?Sized = ZclLinqChannel> {
    config: Arc<CalciforgeConfig>,
    router: Arc<Router>,
    command_handler: Arc<CommandHandler>,
    context_store: ContextStore,
    channel_scanner: Arc<ChannelScanner>,
    transport: Arc<C>,
    choice_state: Option<Arc<ChoiceState>>,
}

impl<C: Channel + ?Sized + 'static> SmsChannel<C> {
    pub fn new(
        config: Arc<CalciforgeConfig>,
        router: Arc<Router>,
        command_handler: Arc<CommandHandler>,
        context_store: ContextStore,
        channel_scanner: Arc<ChannelScanner>,
        transport: Arc<C>,
    ) -> Self {
        Self {
            config,
            router,
            command_handler,
            context_store,
            channel_scanner,
            transport,
            choice_state: None,
        }
    }

    pub fn with_choice_state(mut self, choice_state: Arc<ChoiceState>) -> Self {
        self.choice_state = Some(choice_state);
        self
    }

    fn scan_enabled(&self) -> bool {
        self.config
            .channels
            .iter()
            .find(|c| c.kind == "sms")
            .map(|c| c.scan_messages)
            .unwrap_or(false)
    }

    async fn send_reply(&self, recipient: &str, body: &str) {
        let start = std::time::Instant::now();
        let response_len = body.len();
        match self
            .transport
            .send(&SendMessage::new(body, recipient))
            .await
        {
            Ok(()) => {
                telemetry::reply_sent(
                    "sms",
                    recipient,
                    "reply",
                    response_len,
                    start.elapsed().as_millis() as u64,
                );
            }
            Err(e) => {
                telemetry::reply_failed(
                    "sms",
                    recipient,
                    "reply",
                    start.elapsed().as_millis() as u64,
                    &e,
                );
                warn!(recipient = %recipient, error = %e, "Text/iMessage: failed to send reply");
            }
        }
    }

    async fn send_outbound(&self, recipient: &str, identity_id: &str, message: &OutboundMessage) {
        if let Some(state) = self.choice_state.as_ref() {
            if !message.controls.is_empty() {
                state.record("sms", identity_id, message.controls.clone());
            }
        }
        self.send_reply(recipient, &message.render_text_fallback())
            .await;
    }

    async fn dispatch_resolved_command(
        self: &Arc<Self>,
        identity_id: &str,
        reply_target: &str,
        command: &str,
    ) {
        if CommandHandler::is_switch_command(command) {
            let reply = self.command_handler.handle_switch(command, identity_id);
            self.send_reply(reply_target, &reply).await;
            return;
        }
        if CommandHandler::is_model_command(command) {
            let reply = self.command_handler.handle_model(command, identity_id);
            self.send_reply(reply_target, &reply).await;
            return;
        }
        if CommandHandler::is_approve_command(command) || CommandHandler::is_deny_command(command) {
            if let Some((ack, follow_up)) = self.command_handler.handle_async(command).await {
                self.send_reply(reply_target, &ack).await;
                if let Some(resp) = follow_up {
                    self.send_reply(reply_target, &resp).await;
                }
                return;
            }
        }
        if let Some(reply) = self.command_handler.handle(command) {
            self.send_reply(reply_target, &reply).await;
            return;
        }
        warn!(
            identity = %identity_id,
            command = %command,
            "Text/iMessage: resolved choice command did not match any handler branch — dropped"
        );
    }

    pub async fn handle_message(self: Arc<Self>, msg: ChannelMessage) {
        let received_at = std::time::Instant::now();
        let delivery_lag_ms = telemetry::delivery_lag_ms_from_unix_seconds(msg.timestamp);

        let from = msg.sender.clone();
        let reply_target = if msg.reply_target.is_empty() {
            msg.sender.clone()
        } else {
            msg.reply_target.clone()
        };
        let text = msg.content.clone();

        let identity = match resolve_channel_sender("sms", &from, &self.config) {
            Some(id) => id,
            None => {
                warn!(from = %from, "Text/iMessage: unknown sender - dropping");
                return;
            }
        };

        telemetry::authorized_message("sms", &identity.id, &from, text.len(), delivery_lag_ms);

        let chat_key = conversation_chat_key(&identity.id, &reply_target);

        if self.scan_enabled() {
            let verdict = self
                .channel_scanner
                .scan_text(&text, ScanContext::UserMessage)
                .await;
            match &verdict {
                adversary_detector::verdict::ScanVerdict::Unsafe { reason } => {
                    warn!(
                        identity = %identity.id,
                        reason = %reason,
                        "Text/iMessage: inbound message BLOCKED by adversary scan"
                    );
                    let channel = self.clone();
                    let target = reply_target.clone();
                    let reason_owned = reason.clone();
                    tokio::spawn(async move {
                        channel
                            .send_reply(
                                &target,
                                &format!("Message blocked by security scanner: {reason_owned}"),
                            )
                            .await;
                    });
                    return;
                }
                adversary_detector::verdict::ScanVerdict::Review { reason } => {
                    warn!(
                        identity = %identity.id,
                        reason = %reason,
                        "Text/iMessage: inbound message flagged REVIEW - passing with caution"
                    );
                }
                adversary_detector::verdict::ScanVerdict::Clean => {
                    debug!(identity = %identity.id, "Text/iMessage: inbound scan clean");
                }
            }
        }

        // ── Pending-choice resolution (free-text only) ───────────────────
        if let Some(state) = self.choice_state.as_ref() {
            match state.match_reply("sms", &identity.id, &text) {
                ChoiceMatchResult::Match { command, label, .. } => {
                    info!(
                        identity = %identity.id,
                        label = %label,
                        "Text/iMessage: text reply matched pending choice"
                    );
                    self.dispatch_resolved_command(&identity.id, &reply_target, &command)
                        .await;
                    return;
                }
                ChoiceMatchResult::Ambiguous => {
                    let channel = self.clone();
                    let target = reply_target.clone();
                    tokio::spawn(async move {
                        channel
                            .send_reply(
                                &target,
                                "Multiple options match. Reply with the number, or be more specific.",
                            )
                            .await;
                    });
                    return;
                }
                ChoiceMatchResult::OutOfRange => {
                    let channel = self.clone();
                    let target = reply_target.clone();
                    tokio::spawn(async move {
                        channel
                            .send_reply(
                                &target,
                                "That number isn't one of the options. Reply with a number from the list, or the option name.",
                            )
                            .await;
                    });
                    return;
                }
                _ => {}
            }
        }

        if let Some(reply) = self
            .command_handler
            .agent_choice_message_for_identity(&text, &identity.id)
        {
            let channel = self.clone();
            let target = reply_target.clone();
            let iid = identity.id.clone();
            tokio::spawn(async move {
                channel.send_outbound(&target, &iid, &reply).await;
            });
            return;
        }

        if let Some(reply) = self.command_handler.model_choice_message(&text) {
            let channel = self.clone();
            let target = reply_target.clone();
            let iid = identity.id.clone();
            tokio::spawn(async move {
                channel.send_outbound(&target, &iid, &reply).await;
            });
            return;
        }

        if let Some(reply) = self.command_handler.handle(&text) {
            debug!(identity = %identity.id, cmd = %text.trim(), "Text/iMessage: handled pre-auth command");
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        if CommandHandler::is_command(&text)
            && !CommandHandler::is_status_command(&text)
            && !CommandHandler::is_switch_command(&text)
            && !CommandHandler::is_default_command(&text)
            && !CommandHandler::is_sessions_command(&text)
            && !CommandHandler::is_model_command(&text)
            && !CommandHandler::is_secure_command(&text)
            && !CommandHandler::is_approve_command(&text)
            && !CommandHandler::is_deny_command(&text)
        {
            let reply = self.command_handler.unknown_command(&text);
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        if CommandHandler::is_status_command(&text) {
            let reply = self
                .command_handler
                .cmd_status_for_identity(&identity.id)
                .await;
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        if CommandHandler::is_switch_command(&text) {
            let reply = self.command_handler.handle_switch(&text, &identity.id);
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        if CommandHandler::is_model_command(&text) {
            let reply = self.command_handler.handle_model(&text, &identity.id);
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        if CommandHandler::is_sessions_command(&text) {
            let reply = self
                .command_handler
                .handle_sessions_message(&text, &identity.id)
                .await;
            let channel = self.clone();
            let target = reply_target.clone();
            let iid = identity.id.clone();
            tokio::spawn(async move {
                channel.send_outbound(&target, &iid, &reply).await;
            });
            return;
        }

        if CommandHandler::is_default_command(&text) {
            let reply = self.command_handler.handle_default(&identity.id);
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        if CommandHandler::is_secure_command(&text) {
            debug!(identity = %identity.id, "Text/iMessage: handling secret command");
            if CommandHandler::is_secure_set_command(&text)
                && !crate::config::channel_allows_chat_secret_set(&self.config, "sms")
            {
                let reply = CommandHandler::secure_set_disabled_reply("SMS");
                let channel = self.clone();
                let target = reply_target.clone();
                tokio::spawn(async move {
                    channel.send_reply(&target, &reply).await;
                });
                return;
            }

            let reply = self
                .command_handler
                .handle_secure(&text, &identity.id)
                .await;
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        if text.trim().eq_ignore_ascii_case("!context clear") {
            self.context_store.clear(&chat_key);
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel
                    .send_reply(&target, "Conversation context cleared.")
                    .await;
            });
            return;
        }

        if CommandHandler::is_approve_command(&text) || CommandHandler::is_deny_command(&text) {
            if let Some((ack, follow_up)) = self.command_handler.handle_async(&text).await {
                let channel = self.clone();
                let target = reply_target.clone();
                tokio::spawn(async move {
                    channel.send_reply(&target, &ack).await;
                    if let Some(resp) = follow_up {
                        channel.send_reply(&target, &resp).await;
                    }
                });
                return;
            }
            let reply = self.command_handler.unknown_command(&text);
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        let agent_id = match self.command_handler.active_agent_for(&identity.id) {
            Some(id) => id,
            None => {
                warn!(identity = %identity.id, "Text/iMessage: no routing rule for identity - dropping");
                return;
            }
        };

        let agent = match find_agent(&agent_id, &self.config) {
            Some(a) => a.clone(),
            None => {
                warn!(agent_id = %agent_id, "Text/iMessage: agent not in config");
                let channel = self.clone();
                let target = reply_target.clone();
                tokio::spawn(async move {
                    channel.send_reply(&target, "Agent not configured.").await;
                });
                return;
            }
        };

        let sender_label = self
            .config
            .identities
            .iter()
            .find(|i| i.id == identity.id)
            .and_then(|i| i.display_name.as_deref())
            .unwrap_or(&identity.id)
            .to_string();

        let identity_id = identity.id.clone();
        let model_override = self.command_handler.active_model_for_identity(&identity_id);
        let selected_session = self
            .command_handler
            .active_session_for(&identity_id, &agent_id);
        let preserve_native_commands = crate::adapters::agent_supports_native_commands(&agent);

        tokio::spawn(async move {
            let queue_wait_ms = received_at.elapsed().as_millis() as u64;
            telemetry::agent_dispatch_started("sms", &identity_id, &agent_id, queue_wait_ms);

            let augmented = self.context_store.augment_message_with_options(
                &chat_key,
                &agent_id,
                &text,
                preserve_native_commands,
            );

            let dispatch_start = std::time::Instant::now();
            match self
                .router
                .dispatch_message_with_full_context(
                    &augmented,
                    &agent,
                    &self.config,
                    crate::router::RouterDispatchContext {
                        sender: Some(&identity_id),
                        model_override: model_override.as_deref(),
                        session: selected_session.as_deref(),
                        channel: Some("sms"),
                    },
                )
                .await
            {
                Ok(response) => {
                    let latency_ms = dispatch_start.elapsed().as_millis() as u64;
                    let final_response = response.render_text_fallback();
                    self.command_handler.record_dispatch(latency_ms);
                    telemetry::agent_dispatch_succeeded(
                        "sms",
                        &identity_id,
                        &agent_id,
                        latency_ms,
                        response.response_len(),
                    );

                    debug!(
                        identity = %identity_id,
                        agent_id = %agent_id,
                        response_len = %final_response.len(),
                        attachments = response.attachments.len(),
                        "Text/iMessage: got agent response"
                    );

                    self.context_store.push_with_options(
                        &chat_key,
                        &sender_label,
                        &text,
                        &agent_id,
                        &final_response,
                        preserve_native_commands,
                    );

                    self.send_outbound(&reply_target, &identity_id, &response)
                        .await;
                }
                Err(e) => {
                    if let Some(crate::adapters::AdapterError::ApprovalPending(req)) =
                        e.downcast_ref::<crate::adapters::AdapterError>()
                    {
                        self.command_handler
                            .register_pending_approval(
                                crate::adapters::openclaw::PendingApprovalMeta {
                                    request_id: req.request_id.clone(),
                                    zeroclaw_endpoint: agent.endpoint.clone(),
                                    zeroclaw_auth_token: agent
                                        .auth_token
                                        .clone()
                                        .unwrap_or_default(),
                                    _summary: CommandHandler::approval_request_message(
                                        &req.command,
                                        &req.reason,
                                        &req.request_id,
                                    )
                                    .render_text_fallback(),
                                },
                            )
                            .await;
                        let approval_msg = CommandHandler::approval_request_message(
                            &req.command,
                            &req.reason,
                            &req.request_id,
                        );
                        self.send_outbound(&reply_target, &identity_id, &approval_msg)
                            .await;
                    } else {
                        warn!(identity = %identity_id, error = %e, "Text/iMessage: agent dispatch failed");
                        self.send_reply(&reply_target, &format!("Agent error: {e}"))
                            .await;
                    }
                }
            }
        });
    }
}

fn conversation_chat_key(identity_id: &str, reply_target: &str) -> String {
    format!("sms-{identity_id}-{reply_target}")
}

#[derive(Clone)]
struct WebhookState {
    bridge: Arc<SmsChannel<ZclLinqChannel>>,
    transport: Arc<ZclLinqChannel>,
    signing_secret: Option<String>,
}

async fn health_handler() -> impl IntoResponse {
    Json(json!({ "status": "ok", "channel": "sms" }))
}

async fn webhook_handler(
    State(state): State<WebhookState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Some(secret) = state.signing_secret.as_deref() {
        let timestamp = match headers
            .get("x-webhook-timestamp")
            .and_then(|value| value.to_str().ok())
        {
            Some(value) => value,
            None => return (StatusCode::UNAUTHORIZED, "missing webhook timestamp"),
        };
        let signature = match headers
            .get("x-webhook-signature")
            .and_then(|value| value.to_str().ok())
        {
            Some(value) => value,
            None => return (StatusCode::UNAUTHORIZED, "missing webhook signature"),
        };
        let body_text = match std::str::from_utf8(&body) {
            Ok(value) => value,
            Err(_) => return (StatusCode::BAD_REQUEST, "body must be utf-8 json"),
        };
        if !zeroclaw_channels::linq::verify_linq_signature(secret, body_text, timestamp, signature)
        {
            return (StatusCode::UNAUTHORIZED, "invalid webhook signature");
        }
    }

    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid json"),
    };

    let messages = state.transport.parse_webhook_payload(&payload);
    for msg in messages {
        let bridge = state.bridge.clone();
        tokio::spawn(async move {
            bridge.handle_message(msg).await;
        });
    }

    (StatusCode::OK, "ok")
}

fn read_secret_file(path: &str, label: &str) -> Result<String> {
    Ok(std::fs::read_to_string(expand_tilde(path))
        .with_context(|| format!("Text/iMessage: failed to read {label} '{path}'"))?
        .trim()
        .to_string())
}

fn resolve_optional_secret(
    inline: &Option<String>,
    file: &Option<String>,
    label: &str,
) -> Result<Option<String>> {
    if let Some(path) = file {
        return Ok(Some(read_secret_file(path, label)?));
    }
    Ok(inline.clone().map(|value| value.trim().to_string()))
}

pub async fn run(
    config: Arc<CalciforgeConfig>,
    router: Arc<Router>,
    command_handler: Arc<CommandHandler>,
    context_store: ContextStore,
    channel_scanner: Arc<ChannelScanner>,
    choice_state: Arc<ChoiceState>,
) -> Result<()> {
    let sms_cfg = config
        .channels
        .iter()
        .find(|c| c.kind == "sms" && c.enabled)
        .context("no enabled sms channel found in config")?;

    let api_token = resolve_optional_secret(
        &sms_cfg.sms_linq_api_token,
        &sms_cfg.sms_linq_api_token_file,
        "sms_linq_api_token_file",
    )?
    .filter(|value| !value.is_empty())
    .context("sms_linq_api_token_file or sms_linq_api_token is required for kind = \"sms\"")?;
    let signing_secret = resolve_optional_secret(
        &sms_cfg.sms_linq_signing_secret,
        &sms_cfg.sms_linq_signing_secret_file,
        "sms_linq_signing_secret_file",
    )?
    .filter(|value| !value.is_empty());
    if signing_secret.is_none() {
        warn!(
            "Text/iMessage webhook signature verification is disabled; \
             configure sms_linq_signing_secret_file for public webhook endpoints"
        );
    }
    let from_phone = sms_cfg
        .sms_from_phone
        .as_deref()
        .context("sms_from_phone is required for kind = \"sms\"")?
        .to_string();
    let listen_addr = sms_cfg
        .sms_webhook_listen
        .clone()
        .unwrap_or_else(|| "0.0.0.0:18798".to_string());
    let webhook_path = sms_cfg
        .sms_webhook_path
        .clone()
        .unwrap_or_else(|| "/webhooks/sms".to_string());
    let allowed = sms_cfg.allowed_numbers.clone();

    info!(
        listen = %listen_addr,
        path = %webhook_path,
        from_phone = %from_phone,
        signed = signing_secret.is_some(),
        "Text/iMessage channel starting (Linq webhook receiver)"
    );

    let transport = Arc::new(ZclLinqChannel::new(api_token, from_phone, allowed));
    let bridge = Arc::new(
        SmsChannel::<ZclLinqChannel>::new(
            config,
            router,
            command_handler,
            context_store,
            channel_scanner,
            transport.clone(),
        )
        .with_choice_state(choice_state),
    );

    let state = WebhookState {
        bridge,
        transport,
        signing_secret,
    };
    let app = AxumRouter::new()
        .route("/health", get(health_handler))
        .route(&webhook_path, post(webhook_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&listen_addr)
        .await
        .with_context(|| format!("binding SMS webhook listener on {listen_addr}"))?;

    axum::serve(listener, app)
        .await
        .context("Text/iMessage webhook listener exited")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AgentConfig, CalciforgeConfig, CalciforgeHeader, ChannelAlias, ChannelConfig, Identity,
        RoutingRule,
    };
    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;
    use tokio::sync::mpsc;
    use tokio::sync::Notify;

    struct MockChannel {
        sent: StdMutex<Vec<SendMessage>>,
        sent_notify: Notify,
    }

    impl MockChannel {
        fn new() -> Self {
            Self {
                sent: StdMutex::new(Vec::new()),
                sent_notify: Notify::new(),
            }
        }

        fn drain(&self) -> Vec<SendMessage> {
            std::mem::take(&mut *self.sent.lock().unwrap())
        }

        async fn wait_for_sent_len(&self, expected: usize) {
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                loop {
                    let notified = self.sent_notify.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    if self.sent.lock().unwrap().len() >= expected {
                        return;
                    }
                    notified.await;
                }
            })
            .await
            .expect("timed out waiting for SMS mock send");
        }
    }

    #[async_trait]
    impl Channel for MockChannel {
        fn name(&self) -> &str {
            "mock-sms"
        }

        async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
            self.sent.lock().unwrap().push(message.clone());
            self.sent_notify.notify_waiters();
            Ok(())
        }

        async fn listen(&self, _tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("SMS tests drive handle_message directly"))
        }
    }

    fn make_test_config() -> Arc<CalciforgeConfig> {
        Arc::new(CalciforgeConfig {
            calciforge: CalciforgeHeader { version: 2 },
            identities: vec![Identity {
                id: "alice".to_string(),
                display_name: Some("Alice".to_string()),
                aliases: vec![ChannelAlias {
                    channel: "sms".to_string(),
                    id: "+15555550100".to_string(),
                }],
                role: Some("owner".to_string()),
            }],
            agents: vec![AgentConfig {
                id: "librarian".to_string(),
                kind: "openclaw-channel".to_string(),
                endpoint: "http://127.0.0.1:18789".to_string(),
                ..Default::default()
            }],
            routing: vec![RoutingRule {
                identity: "alice".to_string(),
                default_agent: "librarian".to_string(),
                allowed_agents: vec![],
            }],
            channels: vec![ChannelConfig {
                kind: "sms".to_string(),
                enabled: true,
                allowed_numbers: vec!["+15555550100".to_string()],
                sms_linq_api_token: Some("test-token".to_string()),
                sms_from_phone: Some("+15555550001".to_string()),
                ..Default::default()
            }],
            permissions: None,
            memory: None,
            context: Default::default(),
            model_shortcuts: vec![],
            alloys: vec![],
            cascades: vec![],
            dispatchers: vec![],
            exec_models: vec![],
            security: None,
            proxy: None,
            local_models: None,
        })
    }

    fn make_scanner() -> Arc<ChannelScanner> {
        let security_config = adversary_detector::profiles::SecurityConfig::balanced();
        let scanner =
            adversary_detector::scanner::AdversaryScanner::new(security_config.scanner.clone());
        let audit_logger = adversary_detector::audit::AuditLogger::new("test-sms");
        Arc::new(ChannelScanner::new(scanner, audit_logger, security_config))
    }

    struct TestBridge {
        bridge: Arc<SmsChannel<MockChannel>>,
        _state_dir: tempfile::TempDir,
    }

    fn dummy_bridge_with(config: Arc<CalciforgeConfig>, transport: Arc<MockChannel>) -> TestBridge {
        let router = Arc::new(Router::new());
        let tmp = tempfile::tempdir().expect("tempdir for sms test state isolation");
        let command_handler = Arc::new(CommandHandler::with_state_dir(
            config.clone(),
            tmp.path().to_path_buf(),
        ));
        TestBridge {
            bridge: Arc::new(SmsChannel::<MockChannel>::new(
                config,
                router,
                command_handler,
                ContextStore::new(20, 5),
                make_scanner(),
                transport,
            )),
            _state_dir: tmp,
        }
    }

    #[tokio::test]
    async fn test_handle_message_unknown_sender_drops() {
        let transport = Arc::new(MockChannel::new());
        let bridge = dummy_bridge_with(make_test_config(), transport.clone());

        bridge
            .bridge
            .handle_message(ChannelMessage {
                id: "1".into(),
                sender: "+19990001111".into(),
                reply_target: "+19990001111".into(),
                content: "!ping".into(),
                channel: "linq".into(),
                timestamp: 0,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
            })
            .await;

        assert!(transport.drain().is_empty());
    }

    #[tokio::test]
    async fn test_handle_message_replies_to_chat_id_target() {
        let transport = Arc::new(MockChannel::new());
        let bridge = dummy_bridge_with(make_test_config(), transport.clone());

        bridge
            .bridge
            .handle_message(ChannelMessage {
                id: "1".into(),
                sender: "+15555550100".into(),
                reply_target: "chat_123".into(),
                content: "!ping".into(),
                channel: "linq".into(),
                timestamp: 0,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
            })
            .await;

        transport.wait_for_sent_len(1).await;
        let sent = transport.drain();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].recipient, "chat_123");
    }

    fn make_two_agent_config() -> Arc<CalciforgeConfig> {
        Arc::new(CalciforgeConfig {
            calciforge: CalciforgeHeader { version: 2 },
            identities: vec![Identity {
                id: "alice".to_string(),
                display_name: Some("Alice".to_string()),
                aliases: vec![ChannelAlias {
                    channel: "sms".to_string(),
                    id: "+15555550100".to_string(),
                }],
                role: Some("owner".to_string()),
            }],
            agents: vec![
                AgentConfig {
                    id: "librarian".to_string(),
                    kind: "openclaw-channel".to_string(),
                    endpoint: "http://127.0.0.1:18789".to_string(),
                    ..Default::default()
                },
                AgentConfig {
                    id: "critic".to_string(),
                    kind: "openclaw-channel".to_string(),
                    endpoint: "http://127.0.0.1:18789".to_string(),
                    ..Default::default()
                },
            ],
            routing: vec![RoutingRule {
                identity: "alice".to_string(),
                default_agent: "librarian".to_string(),
                allowed_agents: vec!["librarian".to_string(), "critic".to_string()],
            }],
            channels: vec![ChannelConfig {
                kind: "sms".to_string(),
                enabled: true,
                allowed_numbers: vec!["+15555550100".to_string()],
                sms_linq_api_token: Some("test-token".to_string()),
                sms_from_phone: Some("+15555550001".to_string()),
                ..Default::default()
            }],
            permissions: None,
            memory: None,
            context: Default::default(),
            model_shortcuts: vec![],
            alloys: vec![],
            cascades: vec![],
            dispatchers: vec![],
            exec_models: vec![],
            security: None,
            proxy: None,
            local_models: None,
        })
    }

    fn dummy_bridge_with_choice(
        config: Arc<CalciforgeConfig>,
        transport: Arc<MockChannel>,
        choice_state: Arc<crate::choice_state::ChoiceState>,
    ) -> TestBridge {
        let router = Arc::new(Router::new());
        let tmp = tempfile::tempdir().expect("tempdir for sms test state isolation");
        let command_handler = Arc::new(CommandHandler::with_state_dir(
            config.clone(),
            tmp.path().to_path_buf(),
        ));
        TestBridge {
            bridge: Arc::new(
                SmsChannel::<MockChannel>::new(
                    config,
                    router,
                    command_handler,
                    ContextStore::new(20, 5),
                    make_scanner(),
                    transport,
                )
                .with_choice_state(choice_state),
            ),
            _state_dir: tmp,
        }
    }

    #[tokio::test]
    async fn test_choice_state_free_text_match_dispatches_switch() {
        use crate::choice_state::ChoiceState;
        use crate::messages::{ChoiceControl, ChoiceOption};

        let transport = Arc::new(MockChannel::new());
        let choice_state = Arc::new(ChoiceState::new());
        let bridge = dummy_bridge_with_choice(
            make_two_agent_config(),
            transport.clone(),
            choice_state.clone(),
        );

        let ctrl = ChoiceControl::new(
            "Pick agent",
            vec![
                ChoiceOption::agent("Librarian", "librarian"),
                ChoiceOption::agent("Critic", "critic"),
            ],
        );
        choice_state.record("sms", "alice", vec![ctrl]);

        bridge
            .bridge
            .handle_message(ChannelMessage {
                id: "1".into(),
                sender: "+15555550100".into(),
                reply_target: "+15555550100".into(),
                content: "2".into(),
                channel: "linq".into(),
                timestamp: 0,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
            })
            .await;

        transport.wait_for_sent_len(1).await;
        let sent = transport.drain();
        assert_eq!(sent.len(), 1);
        assert!(
            sent[0].content.contains("critic"),
            "should switch to critic: {}",
            sent[0].content
        );
        assert_eq!(choice_state.pending_len(), 0);
    }

    #[tokio::test]
    async fn test_choice_state_out_of_range_error_reply() {
        use crate::choice_state::ChoiceState;
        use crate::messages::{ChoiceControl, ChoiceOption};

        let transport = Arc::new(MockChannel::new());
        let choice_state = Arc::new(ChoiceState::new());
        let bridge = dummy_bridge_with_choice(
            make_two_agent_config(),
            transport.clone(),
            choice_state.clone(),
        );

        let ctrl = ChoiceControl::new(
            "Pick agent",
            vec![
                ChoiceOption::agent("Librarian", "librarian"),
                ChoiceOption::agent("Critic", "critic"),
            ],
        );
        choice_state.record("sms", "alice", vec![ctrl]);

        bridge
            .bridge
            .handle_message(ChannelMessage {
                id: "1".into(),
                sender: "+15555550100".into(),
                reply_target: "+15555550100".into(),
                content: "99".into(),
                channel: "linq".into(),
                timestamp: 0,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
            })
            .await;

        transport.wait_for_sent_len(1).await;
        let sent = transport.drain();
        assert!(
            sent[0].content.contains("isn't one of the options"),
            "should get out-of-range message: {}",
            sent[0].content
        );
        assert_eq!(choice_state.pending_len(), 1, "state preserved for retry");
    }

    #[tokio::test]
    async fn test_choice_state_no_pending_falls_through() {
        use crate::choice_state::ChoiceState;

        let transport = Arc::new(MockChannel::new());
        let choice_state = Arc::new(ChoiceState::new());
        let bridge =
            dummy_bridge_with_choice(make_test_config(), transport.clone(), choice_state.clone());

        bridge
            .bridge
            .handle_message(ChannelMessage {
                id: "1".into(),
                sender: "+15555550100".into(),
                reply_target: "+15555550100".into(),
                content: "!ping".into(),
                channel: "linq".into(),
                timestamp: 0,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
            })
            .await;

        transport.wait_for_sent_len(1).await;
        let sent = transport.drain();
        assert!(!sent.is_empty(), "should fall through to command handler");
    }

    #[tokio::test]
    async fn test_agents_command_records_controls_then_numeric_selects() {
        use crate::choice_state::ChoiceState;

        let transport = Arc::new(MockChannel::new());
        let choice_state = Arc::new(ChoiceState::new());
        let bridge = dummy_bridge_with_choice(
            make_two_agent_config(),
            transport.clone(),
            choice_state.clone(),
        );

        // Send !agents
        bridge
            .bridge
            .clone()
            .handle_message(ChannelMessage {
                id: "1".into(),
                sender: "+15555550100".into(),
                reply_target: "+15555550100".into(),
                content: "!agents".into(),
                channel: "linq".into(),
                timestamp: 0,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
            })
            .await;

        transport.wait_for_sent_len(1).await;
        let agents_reply = transport.drain();
        assert!(
            !agents_reply.is_empty(),
            "!agents should produce a response"
        );
        assert!(
            choice_state.pending_len() > 0,
            "!agents must record choice controls for text-fallback selection"
        );

        // Reply with "2" to select second agent
        bridge
            .bridge
            .handle_message(ChannelMessage {
                id: "2".into(),
                sender: "+15555550100".into(),
                reply_target: "+15555550100".into(),
                content: "2".into(),
                channel: "linq".into(),
                timestamp: 0,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
            })
            .await;

        transport.wait_for_sent_len(1).await;
        let switch_reply = transport.drain();
        assert_eq!(switch_reply.len(), 1);
        assert!(
            switch_reply[0].content.contains("critic"),
            "selecting option 2 should switch to critic: {}",
            switch_reply[0].content
        );
        assert_eq!(
            choice_state.pending_len(),
            0,
            "pending state should clear after match"
        );
    }

    #[tokio::test]
    async fn test_conversation_ids_do_not_share_context_between_agents() {
        let mut config = (*make_test_config()).clone();
        config.agents = vec![
            AgentConfig {
                id: "librarian".to_string(),
                kind: "artifact-cli".to_string(),
                command: Some("/bin/sh".to_string()),
                args: Some(vec!["-c".to_string(), "cat".to_string()]),
                ..Default::default()
            },
            AgentConfig {
                id: "critic".to_string(),
                kind: "artifact-cli".to_string(),
                command: Some("/bin/sh".to_string()),
                args: Some(vec!["-c".to_string(), "cat".to_string()]),
                ..Default::default()
            },
        ];
        config.routing[0].allowed_agents = vec!["librarian".to_string(), "critic".to_string()];
        let transport = Arc::new(MockChannel::new());
        let bridge = dummy_bridge_with(Arc::new(config), transport.clone());

        bridge
            .bridge
            .clone()
            .handle_message(ChannelMessage {
                id: "1".into(),
                sender: "+15555550100".into(),
                reply_target: "chat_123".into(),
                content: "alpha private context".into(),
                channel: "linq".into(),
                timestamp: 0,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
            })
            .await;
        transport.wait_for_sent_len(1).await;
        let first = transport.drain();
        assert_eq!(first[0].recipient, "chat_123");
        assert!(first[0].content.contains("alpha private context"));

        bridge
            .bridge
            .clone()
            .handle_message(ChannelMessage {
                id: "2".into(),
                sender: "+15555550100".into(),
                reply_target: "chat_456".into(),
                content: "!switch critic".into(),
                channel: "linq".into(),
                timestamp: 0,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
            })
            .await;
        transport.wait_for_sent_len(1).await;
        let switch_reply = transport.drain();
        assert_eq!(switch_reply[0].recipient, "chat_456");

        bridge
            .bridge
            .handle_message(ChannelMessage {
                id: "3".into(),
                sender: "+15555550100".into(),
                reply_target: "chat_456".into(),
                content: "beta fresh prompt".into(),
                channel: "linq".into(),
                timestamp: 0,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
            })
            .await;
        transport.wait_for_sent_len(1).await;
        let second = transport.drain();
        assert_eq!(second[0].recipient, "chat_456");
        assert!(second[0].content.contains("beta fresh prompt"));
        assert!(
            !second[0].content.contains("alpha private context"),
            "chat_456 must not receive chat_123 context: {}",
            second[0].content
        );
        assert!(
            !second[0].content.contains("[Recent context:"),
            "new conversation/agent pair should start without another chat's preamble: {}",
            second[0].content
        );
    }
}
