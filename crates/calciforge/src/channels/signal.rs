//! Signal channel adapter for Calciforge.
//!
//! ## Architecture
//!
//! Calciforge embeds [`zeroclaw_channels::signal::SignalChannel`] directly. The
//! embedded channel talks to a `signal-cli-rest-api` daemon over HTTP/JSON-RPC
//! and surfaces inbound messages via a `tokio::mpsc` `Receiver`. Calciforge
//! drains that receiver, resolves the sender against its identity table,
//! routes through commands / agents, and writes the reply back through the
//! same `Channel::send` interface.
//!
//! ```text
//! Signal user  ⇄  signal-cli-rest-api  ⇄  zeroclawlabs::SignalChannel  ⇄  Calciforge dispatch
//! ```
//!
//! There is **no** webhook receiver in Calciforge for Signal anymore — we no
//! longer terminate HTTP from a ZeroClaw runtime peer. The legacy webhook
//! configuration fields (`zeroclaw_endpoint`, `zeroclaw_auth_token`,
//! `webhook_listen`, `webhook_path`, `webhook_secret`) are rejected at startup
//! when `kind = "signal"`.
//!
//! ## Config
//!
//! ```toml
//! [[channels]]
//! kind = "signal"
//! enabled = true
//! signal_cli_url = "http://127.0.0.1:8080"
//! signal_account = "+15555550001"
//! allowed_numbers = ["+15555550001"]
//! # Optional:
//! # signal_group_id = "group.abc123…"
//! # signal_ignore_attachments = false
//! # signal_ignore_stories = false
//! ```

use crate::sync::Arc;
use anyhow::{anyhow, Context, Result};
use tracing::{debug, info, warn};
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_channels::signal::SignalChannel as ZclSignalChannel;

use crate::{
    auth::{find_agent, resolve_channel_sender},
    choice_state::{
        ChoiceMatchResult, ChoiceState, CHOICE_INDEX_SENTINEL_PREFIX, CHOICE_SENTINEL_PREFIX,
    },
    commands::CommandHandler,
    config::{channel_allows_rich_ui, CalciforgeConfig},
    context::ContextStore,
    messages::OutboundMessage,
    router::Router,
};

use super::telemetry;

use adversary_detector::middleware::ChannelScanner;
use adversary_detector::verdict::ScanContext;

// ---------------------------------------------------------------------------
// Signal channel
// ---------------------------------------------------------------------------

/// Calciforge-side bridge that owns a `zeroclawlabs::SignalChannel`, drains
/// its inbound stream, and dispatches messages through the standard
/// router / command / context pipeline.
pub struct SignalChannel<C: Channel + ?Sized = ZclSignalChannel> {
    config: Arc<CalciforgeConfig>,
    router: Arc<Router>,
    command_handler: Arc<CommandHandler>,
    context_store: ContextStore,
    channel_scanner: Arc<ChannelScanner>,
    /// The actual transport used to send replies. Generic so tests can plug
    /// in a mock; defaults to the concrete `ZclSignalChannel`.
    transport: Arc<C>,
    /// Per-identity pending-choice tracker. Shared across channels so a
    /// reply on one transport can resolve a control sent via another (rare
    /// but real — e.g. user receives a Signal poll, replies via Matrix
    /// to the same identity). When `None`, the channel doesn't record /
    /// resolve choices — used by older test fixtures that pre-date the
    /// matcher.
    choice_state: Option<Arc<ChoiceState>>,
}

impl<C: Channel + ?Sized + 'static> SignalChannel<C> {
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

    /// Attach a `ChoiceState` so this channel records pending choices on
    /// outbound and resolves replies on inbound. Returns `self` for
    /// chained construction.
    pub fn with_choice_state(mut self, choice_state: Arc<ChoiceState>) -> Self {
        self.choice_state = Some(choice_state);
        self
    }

    /// Whether adversary scanning is enabled for the `signal` channel.
    fn scan_enabled(&self) -> bool {
        self.config
            .channels
            .iter()
            .find(|c| c.kind == "signal")
            .map(|c| c.scan_messages)
            .unwrap_or(false)
    }

    /// Best-effort reply send; logs (does not propagate) failures.
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
                    "signal",
                    recipient,
                    "reply",
                    response_len,
                    start.elapsed().as_millis() as u64,
                );
            }
            Err(e) => {
                warn!(recipient = %recipient, error = %e, "Signal: failed to send reply");
            }
        }
    }

    /// Dispatch a command resolved from a pending choice. The resolved
    /// command always comes from a `ChoiceOption` factory method
    /// (agent / model / session / approve / deny) so it's always a
    /// valid `!command` — never freeform input. Run the appropriate
    /// `command_handler` branch directly.
    ///
    /// Skips the adversary scan and the matcher-resolution pass,
    /// which would be redundant — the reply that produced this
    /// command already passed both.
    async fn dispatch_resolved_command(
        self: &Arc<Self>,
        identity_id: &str,
        reply_target: &str,
        command: &str,
    ) {
        let target = reply_target.to_string();

        if CommandHandler::is_switch_command(command) {
            let reply = self.command_handler.handle_switch(command, identity_id);
            self.send_reply(&target, &reply).await;
            return;
        }
        if CommandHandler::is_model_command(command) {
            let reply = self.command_handler.handle_model(command, identity_id);
            self.send_reply(&target, &reply).await;
            return;
        }
        if CommandHandler::is_approve_command(command) || CommandHandler::is_deny_command(command) {
            if let Some((ack, follow_up)) = self.command_handler.handle_async(command).await {
                self.send_reply(&target, &ack).await;
                if let Some(resp) = follow_up {
                    self.send_reply(&target, &resp).await;
                }
                return;
            }
        }
        // Last-resort: defer to the generic handler for unknown shapes.
        // ChoiceOption::agent uses `!agent switch X` which the existing
        // handle_switch path covers; future ChoiceOption factories
        // should land here too. Using `handle` keeps it consistent
        // with the pre-auth command path used elsewhere.
        if let Some(reply) = self.command_handler.handle(command) {
            self.send_reply(&target, &reply).await;
            return;
        }
        warn!(
            identity = %identity_id,
            command = %command,
            "Signal: resolved choice command did not match any command_handler branch — dropped"
        );
    }

    async fn send_outbound(&self, recipient: &str, message: &OutboundMessage) {
        if let (Some(state), Some(control)) = (self.choice_state.as_ref(), message.controls.first())
        {
            state.record("signal", recipient, message.controls.clone());

            if channel_allows_rich_ui(&self.config, "signal") {
                let pairs: Vec<(String, String)> = control
                    .options
                    .iter()
                    .map(|o| {
                        (
                            o.callback_data.clone().unwrap_or_else(|| o.label.clone()),
                            o.label.clone(),
                        )
                    })
                    .collect();
                let prompt = if control.title.trim().is_empty() {
                    message.text.as_deref().unwrap_or("").to_string()
                } else {
                    control.title.clone()
                };
                match self.transport.send_choice(recipient, &prompt, &pairs).await {
                    Ok(()) => {
                        telemetry::reply_sent("signal", recipient, "choice", prompt.len(), 0);
                        return;
                    }
                    Err(e) => {
                        warn!(
                            recipient = %recipient,
                            error = %e,
                            "Signal: send_choice failed, falling back to text"
                        );
                    }
                }
            }
        }
        self.send_reply(recipient, &message.render_text_fallback())
            .await;
    }

    /// Handle a single inbound `ChannelMessage` end-to-end.
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

        // Auth boundary: resolve sender to identity (looks up by E.164 phone).
        let identity = match resolve_channel_sender("signal", &from, &self.config) {
            Some(id) => id,
            None => {
                warn!(from = %from, "Signal: unknown sender — dropping");
                return;
            }
        };

        telemetry::authorized_message("signal", &identity.id, &from, text.len(), delivery_lag_ms);

        let chat_key = format!("signal-{}", identity.id);

        // ── Pending-choice resolution ─────────────────────────────────────
        //
        // Two paths:
        //   1. `[choice]<id-or-title>` sentinel from a native poll vote
        //      (zeroclawlabs fork's process_envelope emits this when
        //      pollAnswer arrives over SSE). Resolve to the original
        //      ChoiceOption via callback_data preferred, label fallback.
        //   2. Free-text reply ("2", "Librarian") on a pending control.
        //      Run the matcher; on Match dispatch the option's command.
        //
        // We do this BEFORE the command fast-paths so a numeric reply
        // doesn't get accidentally interpreted by the freeform parser.
        // Both paths key by reply_target (matches the recipient passed
        // to record() in send_outbound).
        if let Some(state) = self.choice_state.as_ref() {
            if let Some(stripped) = text.strip_prefix(CHOICE_SENTINEL_PREFIX) {
                if let Some(resolved) = state.resolve_sentinel("signal", &reply_target, stripped) {
                    info!(
                        identity = %identity.id,
                        label = %resolved.label,
                        "Signal: poll vote resolved via [choice] sentinel"
                    );
                    self.dispatch_resolved_command(&identity.id, &reply_target, &resolved.command)
                        .await;
                    return;
                }
                // Sentinel arrived but no matching pending state (stale
                // poll, or user deleted-and-replaced the original
                // control). Drop silently — no useful action available.
                debug!(
                    identity = %identity.id,
                    payload = %stripped,
                    "Signal: [choice] sentinel with no matching pending state — ignoring"
                );
                return;
            }
            if let Some(idx_str) = text.strip_prefix(CHOICE_INDEX_SENTINEL_PREFIX) {
                // Index-fallback sentinel (when signal-cli didn't expand
                // selectedTitles). Treat the index as a free-text reply
                // — the matcher's number tier resolves it.
                let idx_reply = idx_str.trim();
                let m = state.match_reply("signal", &reply_target, idx_reply);
                if let ChoiceMatchResult::Match { command, .. } = m {
                    info!(
                        identity = %identity.id,
                        idx = %idx_reply,
                        "Signal: poll vote resolved via [choice-index] sentinel"
                    );
                    self.dispatch_resolved_command(&identity.id, &reply_target, &command)
                        .await;
                    return;
                }
                // Index sentinel that didn't resolve — drop silently.
                return;
            }
            // Free-text matcher path (text-fallback channels also hit
            // this via shared trait).
            match state.match_reply("signal", &reply_target, &text) {
                ChoiceMatchResult::Match { command, label, .. } => {
                    info!(
                        identity = %identity.id,
                        label = %label,
                        "Signal: text reply matched pending choice"
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
                // NoMatch / NoPending / Expired → fall through to normal
                // command/agent dispatch. The user is sending freeform
                // input, not selecting from a stale prompt.
                _ => {}
            }
        }

        // ── Adversary inbound scan ────────────────────────────────────────
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
                        "Signal: inbound message BLOCKED by adversary scan"
                    );
                    let channel = self.clone();
                    let target = reply_target.clone();
                    let reason_owned = reason.clone();
                    tokio::spawn(async move {
                        channel
                            .send_reply(
                                &target,
                                &format!("🚫 Message blocked by security scanner: {reason_owned}"),
                            )
                            .await;
                    });
                    return;
                }
                adversary_detector::verdict::ScanVerdict::Review { reason } => {
                    warn!(
                        identity = %identity.id,
                        reason = %reason,
                        "Signal: inbound message flagged REVIEW — passing with caution"
                    );
                }
                adversary_detector::verdict::ScanVerdict::Clean => {
                    debug!(identity = %identity.id, "Signal: inbound scan clean");
                }
            }
        }

        // ── Command fast-path ─────────────────────────────────────────────

        if let Some(reply) = self
            .command_handler
            .agent_choice_message_for_identity(&text, &identity.id)
        {
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_outbound(&target, &reply).await;
            });
            return;
        }

        if let Some(reply) = self.command_handler.model_choice_message(&text) {
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_outbound(&target, &reply).await;
            });
            return;
        }

        // Pre-auth handler (`!ping`, `!help`, `!agents`, `!metrics`, …)
        if let Some(reply) = self.command_handler.handle(&text) {
            debug!(identity = %identity.id, cmd = %text.trim(), "Signal: handled pre-auth command");
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        // Unknown !command
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

        // !status
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

        // !switch
        if CommandHandler::is_switch_command(&text) {
            let reply = self.command_handler.handle_switch(&text, &identity.id);
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        // !model
        if CommandHandler::is_model_command(&text) {
            let reply = self.command_handler.handle_model(&text, &identity.id);
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        // !sessions
        if CommandHandler::is_sessions_command(&text) {
            let reply = self
                .command_handler
                .handle_sessions_message(&text, &identity.id)
                .await;
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_outbound(&target, &reply).await;
            });
            return;
        }

        // !default
        if CommandHandler::is_default_command(&text) {
            let reply = self.command_handler.handle_default(&identity.id);
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        // !secret / !secure
        if CommandHandler::is_secure_command(&text) {
            debug!(identity = %identity.id, "Signal: handling secret command");
            if CommandHandler::is_secure_set_command(&text)
                && !crate::config::channel_allows_chat_secret_set(&self.config, "signal")
            {
                let reply = CommandHandler::secure_set_disabled_reply("Signal");
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

        // !context clear
        if text.trim().eq_ignore_ascii_case("!context clear") {
            self.context_store.clear(&chat_key);
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel
                    .send_reply(&target, "🧹 Conversation context cleared.")
                    .await;
            });
            return;
        }

        // !approve / !deny
        if CommandHandler::is_approve_command(&text) || CommandHandler::is_deny_command(&text) {
            debug!(identity = %identity.id, cmd = %text.trim(), "Signal: handling async approval command");
            let command_handler = self.command_handler.clone();
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                if let Some((ack, follow_up)) = command_handler.handle_async(&text).await {
                    channel.send_reply(&target, &ack).await;
                    if let Some(resp) = follow_up {
                        channel.send_reply(&target, &resp).await;
                    }
                }
            });
            return;
        }

        // ── Agent dispatch ───────────────────────────────────────────────

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
                let target = reply_target.clone();
                tokio::spawn(async move {
                    channel
                        .send_reply(&target, "⚠️ Agent not configured.")
                        .await;
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
            telemetry::agent_dispatch_started("signal", &identity_id, &agent_id, queue_wait_ms);

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
                        channel: Some("signal"),
                    },
                )
                .await
            {
                Ok(response) => {
                    let latency_ms = dispatch_start.elapsed().as_millis() as u64;
                    let final_response = response.render_text_fallback();
                    self.command_handler.record_dispatch(latency_ms);
                    telemetry::agent_dispatch_succeeded(
                        "signal",
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
                        "Signal: got agent response"
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
                            "Signal: clash approval request - forwarding to user"
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
                    warn!(identity = %identity_id, error = %e, "Signal: agent dispatch failed");
                    self.send_reply(&reply_target, &format!("⚠️ Agent error: {e}"))
                        .await;
                }
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Migration error rendering for legacy webhook config
// ---------------------------------------------------------------------------

const MIGRATION_TOML: &str = r#"
[[channels]]
kind = "signal"
enabled = true
signal_cli_url = "http://127.0.0.1:8080"
signal_account = "+15555550001"
allowed_numbers = ["+15555550001"]
# Optional:
# signal_group_id = "group.abc123…"
# signal_ignore_attachments = false
# signal_ignore_stories = false
"#;

fn migration_error(field: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "Signal channel: legacy webhook field `{field}` is no longer supported. \
         Calciforge now embeds zeroclawlabs::SignalChannel and talks directly \
         to signal-cli-rest-api — no ZeroClaw daemon webhook required. \
         Update your config to the new schema:\n{MIGRATION_TOML}"
    )
}

// ---------------------------------------------------------------------------
// Entry point: spawn embedded SignalChannel listener and dispatch loop
// ---------------------------------------------------------------------------

/// Run the embedded Signal channel. Returns when the listener exits or
/// errors irrecoverably.
pub async fn run(
    config: Arc<CalciforgeConfig>,
    router: Arc<Router>,
    command_handler: Arc<CommandHandler>,
    context_store: ContextStore,
    channel_scanner: Arc<ChannelScanner>,
    choice_state: Arc<ChoiceState>,
) -> Result<()> {
    // Locate the enabled signal channel config block.
    let signal_cfg = config
        .channels
        .iter()
        .find(|c| c.kind == "signal" && c.enabled)
        .context("no enabled signal channel found in config")?;

    // Reject legacy webhook fields with a migration message.
    if signal_cfg.zeroclaw_endpoint.is_some() {
        return Err(migration_error("zeroclaw_endpoint"));
    }
    if signal_cfg.zeroclaw_auth_token.is_some() {
        return Err(migration_error("zeroclaw_auth_token"));
    }
    if signal_cfg.webhook_listen.is_some() {
        return Err(migration_error("webhook_listen"));
    }
    if signal_cfg.webhook_path.is_some() {
        return Err(migration_error("webhook_path"));
    }
    if signal_cfg.webhook_secret.is_some() {
        return Err(migration_error("webhook_secret"));
    }

    let signal_cli_url = signal_cfg
        .signal_cli_url
        .as_deref()
        .context("signal_cli_url is required for kind = \"signal\"")?
        .to_string();
    let signal_account = signal_cfg
        .signal_account
        .as_deref()
        .context("signal_account is required for kind = \"signal\"")?
        .to_string();
    let group_id = signal_cfg.signal_group_id.clone();
    let allowed = signal_cfg.allowed_numbers.clone();
    let ignore_attachments = signal_cfg.signal_ignore_attachments;
    let ignore_stories = signal_cfg.signal_ignore_stories;

    info!(
        url = %signal_cli_url,
        account = %signal_account,
        group = ?group_id,
        "Signal channel starting (embedded zeroclawlabs::SignalChannel)"
    );

    let transport = Arc::new(ZclSignalChannel::new(
        signal_cli_url,
        signal_account,
        group_id,
        allowed,
        ignore_attachments,
        ignore_stories,
    ));

    let bridge = Arc::new(
        SignalChannel::<ZclSignalChannel>::new(
            config,
            router,
            command_handler,
            context_store,
            channel_scanner,
            transport.clone(),
        )
        .with_choice_state(choice_state),
    );

    run_transport_loop(bridge, transport).await
}

async fn run_transport_loop<C>(bridge: Arc<SignalChannel<C>>, transport: Arc<C>) -> Result<()>
where
    C: Channel + ?Sized + 'static,
{
    // Create the inbound channel and start the listener.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ChannelMessage>(64);

    let listener_transport = Arc::clone(&transport);
    let listener_handle = tokio::spawn(async move { listener_transport.listen(tx).await });

    // Drain inbound messages.
    while let Some(msg) = rx.recv().await {
        let bridge = bridge.clone();
        tokio::spawn(async move {
            bridge.handle_message(msg).await;
        });
    }

    // Listener returned (channel closed); join it and surface runtime failures.
    match listener_handle.await {
        Ok(Ok(())) => Err(anyhow!("Signal listener exited unexpectedly")),
        Ok(Err(e)) => Err(e).context("Signal listener exited with error"),
        Err(e) => Err(anyhow!("Signal listener task failed: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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

    /// Test double for `zeroclawlabs::Channel`. Records every `send` call so
    /// tests can assert routing decisions without standing up a real
    /// signal-cli daemon.
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
            .expect("timed out waiting for Signal mock send");
        }
    }

    #[async_trait]
    impl Channel for MockChannel {
        fn name(&self) -> &str {
            "mock-signal"
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
            // Tests drive `handle_message` directly; listen() is a no-op.
            Ok(())
        }
    }

    /// Build a `CalciforgeConfig` with one Signal channel block, optional
    /// legacy webhook fields applied via `mutate`, and one identity aliased
    /// to `"+15555550100"` so dispatch can resolve it.
    fn make_test_config<F: FnOnce(&mut ChannelConfig)>(mutate: F) -> Arc<CalciforgeConfig> {
        let mut channel = ChannelConfig {
            kind: "signal".to_string(),
            enabled: true,
            allowed_numbers: vec!["+15555550100".to_string()],
            signal_cli_url: Some("http://127.0.0.1:8080".to_string()),
            signal_account: Some("+15555550001".to_string()),
            ..Default::default()
        };
        mutate(&mut channel);

        Arc::new(CalciforgeConfig {
            calciforge: CalciforgeHeader { version: 2 },
            identities: vec![Identity {
                id: "alice".to_string(),
                display_name: Some("Alice".to_string()),
                aliases: vec![ChannelAlias {
                    channel: "signal".to_string(),
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
        let audit_logger = adversary_detector::audit::AuditLogger::new("test-signal");
        Arc::new(ChannelScanner::new(scanner, audit_logger, security_config))
    }

    struct TestBridge {
        bridge: Arc<SignalChannel<MockChannel>>,
        _state_dir: tempfile::TempDir,
    }

    fn dummy_bridge_with(config: Arc<CalciforgeConfig>, transport: Arc<MockChannel>) -> TestBridge {
        let router = Arc::new(Router::new());
        let tmp = tempfile::tempdir().expect("tempdir for signal test state isolation");
        let command_handler = Arc::new(CommandHandler::with_state_dir(
            config.clone(),
            tmp.path().to_path_buf(),
        ));
        let context_store = ContextStore::new(20, 5);
        TestBridge {
            bridge: Arc::new(SignalChannel::<MockChannel>::new(
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

    /// `run` should refuse to start with the legacy webhook fields and surface
    /// a migration error pointing the operator at the new schema.
    #[tokio::test]
    async fn test_run_errors_on_old_config_fields() {
        let config = make_test_config(|c| {
            c.zeroclaw_endpoint = Some("http://127.0.0.1:18789".to_string());
        });

        let router = Arc::new(Router::new());
        let tmp = tempfile::tempdir().expect("tempdir for signal test state isolation");
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
            Arc::new(ChoiceState::new()),
        )
        .await
        .expect_err("legacy zeroclaw_endpoint must be rejected");

        let rendered = format!("{err}");
        assert!(
            rendered.contains("zeroclaw_endpoint"),
            "error should name the offending field: {rendered}"
        );
        assert!(
            rendered.contains("signal_cli_url"),
            "error should reference the new schema: {rendered}"
        );
    }

    #[tokio::test]
    async fn test_transport_loop_propagates_listener_error() {
        let config = make_test_config(|_| {});
        let transport = Arc::new(MockChannel::with_listen_error("listen failed"));
        let bridge = dummy_bridge_with(config, Arc::clone(&transport));

        let err = run_transport_loop(bridge.bridge, transport)
            .await
            .expect_err("listener errors must surface from Signal run loop");

        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("listen failed"),
            "error should include listener failure: {rendered}"
        );
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
        assert!(
            rendered.contains("exited unexpectedly"),
            "error should explain the listener stopped: {rendered}"
        );
    }

    /// Unknown senders (no matching identity alias) must be silently dropped —
    /// no reply is sent.
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
            channel: "signal".into(),
            timestamp: 0,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        };

        bridge.bridge.handle_message(msg).await;

        assert!(
            transport.drain().is_empty(),
            "unknown sender must not trigger any send"
        );
    }

    /// Group messages set `reply_target = "group:<id>"`. The bridge must reply
    /// to that target verbatim, never to the raw `sender`.
    #[tokio::test]
    async fn test_handle_message_replies_to_group_target() {
        let config = make_test_config(|_| {});
        let transport = Arc::new(MockChannel::new());
        let bridge = dummy_bridge_with(config, transport.clone());

        let msg = ChannelMessage {
            id: "1".into(),
            sender: "+15555550100".into(),
            reply_target: "group:abc".into(),
            content: "!ping".into(),
            channel: "signal".into(),
            timestamp: 0,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        };

        bridge.bridge.handle_message(msg).await;

        transport.wait_for_sent_len(1).await;

        let sent = transport.drain();
        assert_eq!(sent.len(), 1, "expected exactly one reply, got {sent:?}");
        assert_eq!(
            sent[0].recipient, "group:abc",
            "reply must target the group, not the raw sender"
        );
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
            channel: "signal".into(),
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
            "Signal fallback should include the actionable command: {}",
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
            "Signal fallback should include session action command: {}",
            sent[0].content
        );
        assert!(
            sent[0].content.contains("!approve req-1") && sent[0].content.contains("!deny req-1"),
            "Signal fallback should include both approval commands: {}",
            sent[0].content
        );
    }
}
