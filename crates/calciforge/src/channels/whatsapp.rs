//! WhatsApp channel adapter for Calciforge.
//!
//! Calciforge embeds [`zeroclaw::channels::WhatsAppWebChannel`] directly. The
//! embedded channel owns the WhatsApp Web session, surfaces inbound messages via
//! a `tokio::mpsc` `Receiver`, and sends replies through the same
//! `Channel::send` interface.
//!
//! ```text
//! WhatsApp user  <->  zeroclawlabs::WhatsAppWebChannel  <->  Calciforge dispatch
//! ```
//!
//! There is no webhook receiver in Calciforge for WhatsApp anymore. Legacy
//! ZeroClaw/OpenClaw webhook fields are rejected at startup for `kind =
//! "whatsapp"`.

use crate::sync::Arc;
use anyhow::{anyhow, Context, Result};
use tracing::{debug, info, warn};
use zeroclaw::channels::traits::{Channel, ChannelMessage, SendMessage};
use zeroclaw::channels::WhatsAppWebChannel as ZclWhatsAppWebChannel;

use crate::{
    auth::{find_agent, resolve_channel_sender},
    commands::CommandHandler,
    config::{expand_tilde, CalciforgeConfig, ChannelConfig},
    context::ContextStore,
    messages::OutboundMessage,
    router::Router,
};

use super::telemetry;

use adversary_detector::middleware::ChannelScanner;
use adversary_detector::verdict::ScanContext;

/// Calciforge-side bridge that owns a `zeroclawlabs::WhatsAppWebChannel`,
/// drains its inbound stream, and dispatches messages through the standard
/// router / command / context pipeline.
pub struct WhatsAppChannel<C: Channel + ?Sized = ZclWhatsAppWebChannel> {
    config: Arc<CalciforgeConfig>,
    router: Arc<Router>,
    command_handler: Arc<CommandHandler>,
    context_store: ContextStore,
    channel_scanner: Arc<ChannelScanner>,
    transport: Arc<C>,
}

impl<C: Channel + ?Sized + 'static> WhatsAppChannel<C> {
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
        }
    }

    fn scan_enabled(&self) -> bool {
        self.config
            .channels
            .iter()
            .find(|c| c.kind == "whatsapp")
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
                    "whatsapp",
                    recipient,
                    "reply",
                    response_len,
                    start.elapsed().as_millis() as u64,
                );
            }
            Err(e) => {
                telemetry::reply_failed(
                    "whatsapp",
                    recipient,
                    "reply",
                    start.elapsed().as_millis() as u64,
                    &e,
                );
                warn!(recipient = %recipient, error = %e, "WhatsApp: failed to send reply");
            }
        }
    }

    async fn send_outbound(&self, recipient: &str, message: &OutboundMessage) {
        self.send_reply(recipient, &message.render_text_fallback())
            .await;
    }

    fn command_reply_ready(
        &self,
        identity_id: &str,
        command_kind: &'static str,
        received_at: std::time::Instant,
        handler_latency_ms: u64,
        response_len: usize,
    ) {
        telemetry::command_reply_ready(
            "whatsapp",
            identity_id,
            command_kind,
            received_at.elapsed().as_millis() as u64,
            handler_latency_ms,
            response_len,
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

        let identity = match resolve_channel_sender("whatsapp", &from, &self.config) {
            Some(id) => id,
            None => {
                warn!(from = %from, "WhatsApp: unknown sender - dropping");
                return;
            }
        };

        telemetry::authorized_message("whatsapp", &identity.id, &from, text.len(), delivery_lag_ms);

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
                        "WhatsApp: inbound message BLOCKED by adversary scan"
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
                        "WhatsApp: inbound message flagged REVIEW - passing with caution"
                    );
                }
                adversary_detector::verdict::ScanVerdict::Clean => {
                    debug!(identity = %identity.id, "WhatsApp: inbound scan clean");
                }
            }
        }

        let command_start = std::time::Instant::now();
        if let Some(reply) = self
            .command_handler
            .agent_choice_message_for_identity(&text, &identity.id)
        {
            self.command_reply_ready(
                &identity.id,
                "agent_choices",
                received_at,
                command_start.elapsed().as_millis() as u64,
                reply.response_len(),
            );
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_outbound(&target, &reply).await;
            });
            return;
        }

        if let Some(reply) = self.command_handler.model_choice_message(&text) {
            self.command_reply_ready(
                &identity.id,
                "model_choices",
                received_at,
                command_start.elapsed().as_millis() as u64,
                reply.response_len(),
            );
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_outbound(&target, &reply).await;
            });
            return;
        }

        if let Some(reply) = self.command_handler.handle(&text) {
            debug!(identity = %identity.id, cmd = %text.trim(), "WhatsApp: handled pre-auth command");
            self.command_reply_ready(
                &identity.id,
                "pre_auth",
                received_at,
                command_start.elapsed().as_millis() as u64,
                reply.len(),
            );
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
            let command_start = std::time::Instant::now();
            let reply = self.command_handler.unknown_command(&text);
            self.command_reply_ready(
                &identity.id,
                "unknown",
                received_at,
                command_start.elapsed().as_millis() as u64,
                reply.len(),
            );
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        if CommandHandler::is_status_command(&text) {
            let command_start = std::time::Instant::now();
            let reply = self
                .command_handler
                .cmd_status_for_identity(&identity.id)
                .await;
            self.command_reply_ready(
                &identity.id,
                "status",
                received_at,
                command_start.elapsed().as_millis() as u64,
                reply.len(),
            );
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        if CommandHandler::is_switch_command(&text) {
            let command_start = std::time::Instant::now();
            let reply = self.command_handler.handle_switch(&text, &identity.id);
            self.command_reply_ready(
                &identity.id,
                "switch",
                received_at,
                command_start.elapsed().as_millis() as u64,
                reply.len(),
            );
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        if CommandHandler::is_model_command(&text) {
            let command_start = std::time::Instant::now();
            let reply = self.command_handler.handle_model(&text, &identity.id);
            self.command_reply_ready(
                &identity.id,
                "model",
                received_at,
                command_start.elapsed().as_millis() as u64,
                reply.len(),
            );
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        if CommandHandler::is_sessions_command(&text) {
            let command_start = std::time::Instant::now();
            let reply = self
                .command_handler
                .handle_sessions_message(&text, &identity.id)
                .await;
            self.command_reply_ready(
                &identity.id,
                "sessions",
                received_at,
                command_start.elapsed().as_millis() as u64,
                reply.response_len(),
            );
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_outbound(&target, &reply).await;
            });
            return;
        }

        if CommandHandler::is_default_command(&text) {
            let command_start = std::time::Instant::now();
            let reply = self.command_handler.handle_default(&identity.id);
            self.command_reply_ready(
                &identity.id,
                "default",
                received_at,
                command_start.elapsed().as_millis() as u64,
                reply.len(),
            );
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        if CommandHandler::is_secure_command(&text) {
            debug!(identity = %identity.id, "WhatsApp: handling secret command");
            if CommandHandler::is_secure_set_command(&text)
                && !crate::config::channel_allows_chat_secret_set(&self.config, "whatsapp")
            {
                let reply = CommandHandler::secure_set_disabled_reply("WhatsApp");
                self.command_reply_ready(
                    &identity.id,
                    "secure_disabled",
                    received_at,
                    0,
                    reply.len(),
                );
                let channel = self.clone();
                let target = reply_target.clone();
                tokio::spawn(async move {
                    channel.send_reply(&target, &reply).await;
                });
                return;
            }

            let command_start = std::time::Instant::now();
            let reply = self
                .command_handler
                .handle_secure(&text, &identity.id)
                .await;
            self.command_reply_ready(
                &identity.id,
                "secure",
                received_at,
                command_start.elapsed().as_millis() as u64,
                reply.len(),
            );
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        if text.trim().eq_ignore_ascii_case("!context clear") {
            self.context_store.clear(&chat_key);
            self.command_reply_ready(
                &identity.id,
                "context_clear",
                received_at,
                0,
                "Conversation context cleared.".len(),
            );
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
            let command_start = std::time::Instant::now();
            let command_handler = self.command_handler.clone();
            let channel = self.clone();
            let target = reply_target.clone();
            let identity_id = identity.id.clone();
            tokio::spawn(async move {
                if let Some((ack, follow_up)) = command_handler.handle_async(&text).await {
                    channel.command_reply_ready(
                        &identity_id,
                        "approval",
                        received_at,
                        command_start.elapsed().as_millis() as u64,
                        ack.len() + follow_up.as_ref().map(|s| s.len()).unwrap_or(0),
                    );
                    channel.send_reply(&target, &ack).await;
                    if let Some(resp) = follow_up {
                        channel.send_reply(&target, &resp).await;
                    }
                }
            });
            return;
        }

        let agent_id = match self.command_handler.active_agent_for(&identity.id) {
            Some(id) => id,
            None => {
                warn!(identity = %identity.id, "WhatsApp: no routing rule for identity - dropping");
                return;
            }
        };

        let agent = match find_agent(&agent_id, &self.config) {
            Some(a) => a.clone(),
            None => {
                warn!(agent_id = %agent_id, "WhatsApp: agent not in config");
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
            telemetry::agent_dispatch_started("whatsapp", &identity_id, &agent_id, queue_wait_ms);

            let augmented = self.context_store.augment_message_with_options(
                &chat_key,
                &agent_id,
                &text,
                preserve_native_commands,
            );

            let dispatch_start = std::time::Instant::now();
            match self
                .router
                .dispatch_message_with_sender_model_and_session(
                    &augmented,
                    &agent,
                    &self.config,
                    Some(&identity_id),
                    model_override.as_deref(),
                    selected_session.as_deref(),
                )
                .await
            {
                Ok(response) => {
                    let latency_ms = dispatch_start.elapsed().as_millis() as u64;
                    let final_response = response.render_text_fallback();
                    self.command_handler.record_dispatch(latency_ms);
                    telemetry::agent_dispatch_succeeded(
                        "whatsapp",
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
                        "WhatsApp: got agent response"
                    );

                    self.context_store.push_with_options(
                        &chat_key,
                        &sender_label,
                        &text,
                        &agent_id,
                        &final_response,
                        preserve_native_commands,
                    );

                    self.send_outbound(&reply_target, &response).await;
                }
                Err(e) => {
                    if let Some(crate::adapters::AdapterError::ApprovalPending(req)) =
                        e.downcast_ref::<crate::adapters::AdapterError>()
                    {
                        let req = req.clone();
                        debug!(
                            request_id = %req.request_id,
                            command = %req.command,
                            "WhatsApp: clash approval request - forwarding to user"
                        );
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
                        let notification = CommandHandler::approval_request_message(
                            &req.command,
                            &req.reason,
                            &req.request_id,
                        );
                        self.send_outbound(&reply_target, &notification).await;
                        return;
                    }
                    warn!(identity = %identity_id, error = %e, "WhatsApp: agent dispatch failed");
                    self.send_reply(&reply_target, &format!("Agent error: {e}"))
                        .await;
                }
            }
        });
    }
}

fn conversation_chat_key(identity_id: &str, reply_target: &str) -> String {
    format!("whatsapp-{identity_id}-{reply_target}")
}

const MIGRATION_TOML: &str = r#"
[[channels]]
kind = "whatsapp"
enabled = true
whatsapp_session_path = "~/.config/calciforge/whatsapp/session.db"
allowed_numbers = ["+15555550001"]
# Optional pairing-code login:
# whatsapp_pair_phone = "15555550001"
"#;

fn migration_error(field: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "WhatsApp channel: legacy webhook field `{field}` is no longer supported. \
         Calciforge now embeds zeroclawlabs::WhatsAppWebChannel and owns the \
         WhatsApp Web session directly. Update your config to the new schema:\n{MIGRATION_TOML}"
    )
}

fn resolved_session_path(config: &ChannelConfig) -> Result<String> {
    let configured = config
        .whatsapp_session_path
        .as_deref()
        .context("whatsapp_session_path is required for kind = \"whatsapp\"")?;
    Ok(expand_tilde(configured).display().to_string())
}

pub async fn run(
    config: Arc<CalciforgeConfig>,
    router: Arc<Router>,
    command_handler: Arc<CommandHandler>,
    context_store: ContextStore,
    channel_scanner: Arc<ChannelScanner>,
) -> Result<()> {
    let whatsapp_cfg = config
        .channels
        .iter()
        .find(|c| c.kind == "whatsapp" && c.enabled)
        .context("no enabled whatsapp channel found in config")?;

    if whatsapp_cfg.zeroclaw_endpoint.is_some() {
        return Err(migration_error("zeroclaw_endpoint"));
    }
    if whatsapp_cfg.zeroclaw_auth_token.is_some() {
        return Err(migration_error("zeroclaw_auth_token"));
    }
    if whatsapp_cfg.webhook_listen.is_some() {
        return Err(migration_error("webhook_listen"));
    }
    if whatsapp_cfg.webhook_path.is_some() {
        return Err(migration_error("webhook_path"));
    }
    if whatsapp_cfg.webhook_secret.is_some() {
        return Err(migration_error("webhook_secret"));
    }

    let session_path = resolved_session_path(whatsapp_cfg)?;
    let pair_phone = whatsapp_cfg.whatsapp_pair_phone.clone();
    let pair_code = whatsapp_cfg.whatsapp_pair_code.clone();
    let allowed = whatsapp_cfg.allowed_numbers.clone();
    let mention_only = whatsapp_cfg.whatsapp_mention_only;
    let mode = whatsapp_cfg.whatsapp_mode.clone();
    let dm_policy = whatsapp_cfg.whatsapp_dm_policy.clone();
    let group_policy = whatsapp_cfg.whatsapp_group_policy.clone();
    let self_chat_mode = whatsapp_cfg.whatsapp_self_chat_mode;
    let dm_mention_patterns = whatsapp_cfg.whatsapp_dm_mention_patterns.clone();
    let group_mention_patterns = whatsapp_cfg.whatsapp_group_mention_patterns.clone();

    info!(
        session_path = %session_path,
        pair_phone = ?pair_phone,
        mode = ?mode,
        mention_only,
        "WhatsApp channel starting (embedded zeroclawlabs::WhatsAppWebChannel)"
    );

    let transport = Arc::new(
        ZclWhatsAppWebChannel::new(
            session_path,
            pair_phone,
            pair_code,
            allowed,
            mention_only,
            mode,
            dm_policy,
            group_policy,
            self_chat_mode,
        )
        .with_dm_mention_patterns(dm_mention_patterns)
        .with_group_mention_patterns(group_mention_patterns),
    );

    let bridge = Arc::new(WhatsAppChannel::<ZclWhatsAppWebChannel>::new(
        config,
        router,
        command_handler,
        context_store,
        channel_scanner,
        transport.clone(),
    ));

    run_transport_loop(bridge, transport).await
}

async fn run_transport_loop<C>(bridge: Arc<WhatsAppChannel<C>>, transport: Arc<C>) -> Result<()>
where
    C: Channel + ?Sized + 'static,
{
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ChannelMessage>(64);

    let listener_transport = Arc::clone(&transport);
    let listener_handle = tokio::spawn(async move { listener_transport.listen(tx).await });

    while let Some(msg) = rx.recv().await {
        let bridge = bridge.clone();
        tokio::spawn(async move {
            bridge.handle_message(msg).await;
        });
    }

    match listener_handle.await {
        Ok(Ok(())) => Err(anyhow!("WhatsApp listener exited unexpectedly")),
        Ok(Err(e)) => Err(e).context("WhatsApp listener exited with error"),
        Err(e) => Err(anyhow!("WhatsApp listener task failed: {e}")),
    }
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
        listen_error: StdMutex<Option<String>>,
    }

    impl MockChannel {
        fn new() -> Self {
            Self {
                sent: StdMutex::new(Vec::new()),
                sent_notify: Notify::new(),
                listen_error: StdMutex::new(None),
            }
        }

        fn with_listen_error(error: &str) -> Self {
            Self {
                sent: StdMutex::new(Vec::new()),
                sent_notify: Notify::new(),
                listen_error: StdMutex::new(Some(error.to_string())),
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
            .expect("timed out waiting for WhatsApp mock send");
        }
    }

    #[async_trait]
    impl Channel for MockChannel {
        fn name(&self) -> &str {
            "mock-whatsapp"
        }

        async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
            self.sent.lock().unwrap().push(message.clone());
            self.sent_notify.notify_waiters();
            Ok(())
        }

        async fn listen(&self, _tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
            if let Some(error) = self.listen_error.lock().unwrap().take() {
                return Err(anyhow::anyhow!(error));
            }
            Ok(())
        }
    }

    fn make_test_config<F: FnOnce(&mut ChannelConfig)>(mutate: F) -> Arc<CalciforgeConfig> {
        let mut channel = ChannelConfig {
            kind: "whatsapp".to_string(),
            enabled: true,
            allowed_numbers: vec!["+15555550100".to_string()],
            whatsapp_session_path: Some("test-session.db".to_string()),
            ..Default::default()
        };
        mutate(&mut channel);

        Arc::new(CalciforgeConfig {
            calciforge: CalciforgeHeader { version: 2 },
            identities: vec![Identity {
                id: "alice".to_string(),
                display_name: Some("Alice".to_string()),
                aliases: vec![ChannelAlias {
                    channel: "whatsapp".to_string(),
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
            channels: vec![channel],
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
        let audit_logger = adversary_detector::audit::AuditLogger::new("test-whatsapp");
        Arc::new(ChannelScanner::new(scanner, audit_logger, security_config))
    }

    struct TestBridge {
        bridge: Arc<WhatsAppChannel<MockChannel>>,
        _state_dir: tempfile::TempDir,
    }

    fn dummy_bridge_with(config: Arc<CalciforgeConfig>, transport: Arc<MockChannel>) -> TestBridge {
        let router = Arc::new(Router::new());
        let tmp = tempfile::tempdir().expect("tempdir for whatsapp test state isolation");
        let command_handler = Arc::new(CommandHandler::with_state_dir(
            config.clone(),
            tmp.path().to_path_buf(),
        ));
        let context_store = ContextStore::new(20, 5);
        TestBridge {
            bridge: Arc::new(WhatsAppChannel::<MockChannel>::new(
                config,
                router,
                command_handler,
                context_store,
                make_scanner(),
                transport,
            )),
            _state_dir: tmp,
        }
    }

    #[tokio::test]
    async fn test_run_errors_on_old_config_fields() {
        let config = make_test_config(|c| {
            c.zeroclaw_endpoint = Some("http://127.0.0.1:18789".to_string());
        });

        let router = Arc::new(Router::new());
        let tmp = tempfile::tempdir().expect("tempdir for whatsapp test state isolation");
        let command_handler = Arc::new(CommandHandler::with_state_dir(
            config.clone(),
            tmp.path().to_path_buf(),
        ));
        let context_store = ContextStore::new(20, 5);
        let channel_scanner = make_scanner();

        let err = run(
            config,
            router,
            command_handler,
            context_store,
            channel_scanner,
        )
        .await
        .expect_err("legacy zeroclaw_endpoint must be rejected");

        let rendered = format!("{err}");
        assert!(rendered.contains("zeroclaw_endpoint"));
        assert!(rendered.contains("whatsapp_session_path"));
    }

    #[test]
    fn test_session_path_expands_tilde() {
        let mut config = ChannelConfig {
            whatsapp_session_path: Some("~/.config/calciforge/whatsapp/session.db".to_string()),
            ..Default::default()
        };

        let session_path = resolved_session_path(&config)
            .expect("configured WhatsApp session path should resolve");

        assert!(
            !session_path.starts_with("~/"),
            "WhatsApp session path should not keep a literal tilde: {session_path}"
        );
        assert!(
            session_path.ends_with(".config/calciforge/whatsapp/session.db"),
            "WhatsApp session path should preserve the configured suffix: {session_path}"
        );

        config.whatsapp_session_path = Some("/var/lib/calciforge/wa.db".to_string());
        assert_eq!(
            resolved_session_path(&config).expect("absolute session path should resolve"),
            "/var/lib/calciforge/wa.db"
        );
    }

    #[tokio::test]
    async fn test_transport_loop_propagates_listener_error() {
        let config = make_test_config(|_| {});
        let transport = Arc::new(MockChannel::with_listen_error("listen failed"));
        let bridge = dummy_bridge_with(config, Arc::clone(&transport));

        let err = run_transport_loop(bridge.bridge, transport)
            .await
            .expect_err("listener errors must surface from WhatsApp run loop");

        let rendered = format!("{err:#}");
        assert!(rendered.contains("listen failed"));
    }

    #[tokio::test]
    async fn test_transport_loop_errors_on_clean_listener_exit() {
        let config = make_test_config(|_| {});
        let transport = Arc::new(MockChannel::new());
        let bridge = dummy_bridge_with(config, Arc::clone(&transport));

        let err = run_transport_loop(bridge.bridge, transport)
            .await
            .expect_err("clean listener exits are unexpected in production");

        let rendered = format!("{err:#}");
        assert!(rendered.contains("exited unexpectedly"));
    }

    #[tokio::test]
    async fn test_handle_message_unknown_sender_drops() {
        let config = make_test_config(|_| {});
        let transport = Arc::new(MockChannel::new());
        let bridge = dummy_bridge_with(config, transport.clone());

        let msg = ChannelMessage {
            id: "1".into(),
            sender: "+19990001111".into(),
            reply_target: "+19990001111".into(),
            content: "!ping".into(),
            channel: "whatsapp".into(),
            timestamp: 0,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        };

        bridge.bridge.handle_message(msg).await;

        assert!(transport.drain().is_empty());
    }

    #[tokio::test]
    async fn test_handle_message_replies_to_group_target() {
        let config = make_test_config(|_| {});
        let transport = Arc::new(MockChannel::new());
        let bridge = dummy_bridge_with(config, transport.clone());

        let msg = ChannelMessage {
            id: "1".into(),
            sender: "+15555550100".into(),
            reply_target: "12345@g.us".into(),
            content: "!ping".into(),
            channel: "whatsapp".into(),
            timestamp: 0,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        };

        bridge.bridge.handle_message(msg).await;

        transport.wait_for_sent_len(1).await;

        let sent = transport.drain();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].recipient, "12345@g.us");
    }

    #[tokio::test]
    async fn test_agent_choices_render_text_fallback() {
        let config = make_test_config(|c| {
            c.ui_mode = crate::config::ChannelUiMode::Text;
        });
        let transport = Arc::new(MockChannel::new());
        let bridge = dummy_bridge_with(config, transport.clone());

        let msg = ChannelMessage {
            id: "1".into(),
            sender: "+15555550100".into(),
            reply_target: "+15555550100".into(),
            content: "!agents".into(),
            channel: "whatsapp".into(),
            timestamp: 0,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        };

        bridge.bridge.handle_message(msg).await;
        transport.wait_for_sent_len(1).await;

        let sent = transport.drain();
        assert_eq!(sent.len(), 1);
        assert!(
            sent[0].content.contains("!agent switch librarian"),
            "WhatsApp fallback should include the actionable command: {}",
            sent[0].content
        );
    }

    #[tokio::test]
    async fn test_choice_controls_render_text_fallback_for_sessions_and_approvals() {
        let config = make_test_config(|c| {
            c.ui_mode = crate::config::ChannelUiMode::Text;
        });
        let transport = Arc::new(MockChannel::new());
        let bridge = dummy_bridge_with(config, transport.clone());

        let message = OutboundMessage::text("Choose")
            .with_control(crate::messages::ChoiceControl::new(
                "Attach to a session",
                vec![crate::messages::ChoiceOption::session(
                    "backend",
                    "claude-acpx",
                    "backend",
                )],
            ))
            .with_control(crate::messages::ChoiceControl::new(
                "Choose an approval action",
                vec![
                    crate::messages::ChoiceOption::approve("req-1"),
                    crate::messages::ChoiceOption::deny("req-1"),
                ],
            ));

        bridge.bridge.send_outbound("+15555550100", &message).await;
        transport.wait_for_sent_len(1).await;

        let sent = transport.drain();
        assert_eq!(sent.len(), 1);
        assert!(
            sent[0].content.contains("!switch claude-acpx backend"),
            "WhatsApp fallback should include session action command: {}",
            sent[0].content
        );
        assert!(
            sent[0].content.contains("!approve req-1") && sent[0].content.contains("!deny req-1"),
            "WhatsApp fallback should include both approval commands: {}",
            sent[0].content
        );
    }

    #[tokio::test]
    async fn test_group_targets_do_not_share_context_between_agents() {
        let mut config = (*make_test_config(|_| {})).clone();
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
        let config = Arc::new(config);
        let transport = Arc::new(MockChannel::new());
        let bridge = dummy_bridge_with(config, transport.clone());

        bridge
            .bridge
            .clone()
            .handle_message(ChannelMessage {
                id: "1".into(),
                sender: "+15555550100".into(),
                reply_target: "group-a@g.us".into(),
                content: "alpha private context".into(),
                channel: "whatsapp".into(),
                timestamp: 0,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
            })
            .await;
        transport.wait_for_sent_len(1).await;
        let first = transport.drain();
        assert_eq!(first[0].recipient, "group-a@g.us");
        assert!(first[0].content.contains("alpha private context"));

        bridge
            .bridge
            .clone()
            .handle_message(ChannelMessage {
                id: "2".into(),
                sender: "+15555550100".into(),
                reply_target: "group-b@g.us".into(),
                content: "!switch critic".into(),
                channel: "whatsapp".into(),
                timestamp: 0,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
            })
            .await;
        transport.wait_for_sent_len(1).await;
        let switch_reply = transport.drain();
        assert_eq!(switch_reply[0].recipient, "group-b@g.us");

        bridge
            .bridge
            .handle_message(ChannelMessage {
                id: "3".into(),
                sender: "+15555550100".into(),
                reply_target: "group-b@g.us".into(),
                content: "beta fresh prompt".into(),
                channel: "whatsapp".into(),
                timestamp: 0,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
            })
            .await;
        transport.wait_for_sent_len(1).await;
        let second = transport.drain();
        assert_eq!(second[0].recipient, "group-b@g.us");
        assert!(second[0].content.contains("beta fresh prompt"));
        assert!(
            !second[0].content.contains("alpha private context"),
            "group B must not receive group A context: {}",
            second[0].content
        );
        assert!(
            !second[0].content.contains("[Recent context:"),
            "new group/agent pair should start without another group's preamble: {}",
            second[0].content
        );
    }

    #[tokio::test]
    async fn test_handle_message_renders_artifact_fallback() {
        let mut config = (*make_test_config(|_| {})).clone();
        config.agents = vec![AgentConfig {
            id: "librarian".to_string(),
            kind: "artifact-cli".to_string(),
            command: Some("/bin/sh".to_string()),
            args: Some(vec![
                "-c".to_string(),
                "cat >/dev/null; printf 'image-bytes' > \"$CALCIFORGE_ARTIFACT_DIR/result.png\"; printf 'done\\n'"
                    .to_string(),
            ]),
            ..Default::default()
        }];
        let config = Arc::new(config);
        let transport = Arc::new(MockChannel::new());
        let bridge = dummy_bridge_with(config, transport.clone());

        let msg = ChannelMessage {
            id: "1".into(),
            sender: "+15555550100".into(),
            reply_target: "+15555550100".into(),
            content: "make an image".into(),
            channel: "whatsapp".into(),
            timestamp: 0,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        };

        bridge.bridge.handle_message(msg).await;
        transport.wait_for_sent_len(1).await;

        let sent = transport.drain();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].content.contains("done"));
        assert!(sent[0].content.contains("Attachments:"));
        assert!(sent[0].content.contains("result.png"));
        assert!(
            !sent[0].content.contains("/tmp/calciforge-artifacts"),
            "fallback must not leak local artifact paths: {}",
            sent[0].content
        );
    }
}
