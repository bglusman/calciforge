//! Telegram channel adapter using teloxide.
//!
//! Listens for messages via long polling, enforces the allow_list at the
//! boundary, routes to the downstream agent, and sends the reply back.

use anyhow::{Context, Result};
use teloxide::{
    prelude::*,
    types::{ChatId, InlineKeyboardButton, InlineKeyboardMarkup, InputFile, Me, ParseMode},
};
use tracing::{debug, info, warn};

use crate::sync::Arc;

use crate::{
    auth::{find_agent, resolve_telegram_sender},
    choice_state::{ChoiceMatchResult, ChoiceState},
    commands::CommandHandler,
    config::{channel_allows_rich_ui, expand_tilde, CalciforgeConfig},
    context::ContextStore,
    messages::{AttachmentKind, OutboundAttachment, OutboundMessage},
    router::Router,
};

use super::telemetry;

/// Run the Telegram bot until shutdown.
pub async fn run(
    config: Arc<CalciforgeConfig>,
    router: Arc<Router>,
    command_handler: Arc<CommandHandler>,
    context_store: ContextStore,
    choice_state: Arc<ChoiceState>,
) -> Result<()> {
    // Find the Telegram channel config
    let tg_channel = config
        .channels
        .iter()
        .find(|c| c.kind == "telegram" && c.enabled)
        .context("no enabled telegram channel found in config")?;

    // Read bot token from file
    let token_file_path = tg_channel
        .bot_token_file
        .as_ref()
        .context("telegram channel missing bot_token_file")?;
    let token_path = expand_tilde(token_file_path);
    let token = std::fs::read_to_string(&token_path)
        .with_context(|| format!("reading Telegram bot token from {}", token_path.display()))?
        .trim()
        .to_string();

    let bot = Bot::new(token);

    let me: Me = bot.get_me().await.context("failed to get bot info")?;
    info!(username = %me.username(), "Telegram bot connected");

    let config_clone = config.clone();
    let router_clone = router.clone();
    let cmd_handler_clone = command_handler.clone();
    let ctx_store_clone = context_store.clone();
    let cs_clone = choice_state.clone();

    let callback_config = config.clone();
    let callback_cmd_handler = command_handler.clone();

    let message_handler =
        Update::filter_message().branch(dptree::entry().endpoint(move |bot: Bot, msg: Message| {
            let cfg = config_clone.clone();
            let rtr = router_clone.clone();
            let cmd = cmd_handler_clone.clone();
            let ctx = ctx_store_clone.clone();
            let cs = cs_clone.clone();
            async move {
                handle_message_nonblocking(bot, msg, cfg, rtr, cmd, ctx, cs);
                respond(())
            }
        }));

    let callback_handler = Update::filter_callback_query().branch(dptree::entry().endpoint(
        move |bot: Bot, query: CallbackQuery| {
            let cfg = callback_config.clone();
            let cmd = callback_cmd_handler.clone();
            async move {
                handle_callback_query(bot, query, cfg, cmd).await;
                respond(())
            }
        },
    ));

    let handler = dptree::entry()
        .branch(message_handler)
        .branch(callback_handler);

    Dispatcher::builder(bot, handler)
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    Ok(())
}

/// Non-blocking message handler.
///
/// Handles commands synchronously (before returning) and spawns agent dispatch
/// in a detached tokio task.  This prevents Teloxide's per-chat serialisation
/// from blocking `!commands` behind a slow agent call.
///
/// Message flow:
/// 1. Extract text + sender (synchronous)
/// 2. Auth — unknown sender → drop silently (synchronous)
/// 3. Build per-identity context key (synchronous)
/// 4. Pre-auth commands (`!ping`, `!help`, `!agents`, `!metrics`) — spawn reply task, return immediately
/// 5. `!status` — post-auth: shows per-identity active agent, spawn reply task, return immediately
/// 6. `!switch <agent>` — post-auth: spawn reply task, return immediately
/// 7. `!context clear` — spawn reply task, return immediately
/// 8. Resolve active agent (synchronous)
/// 9. Spawn agent dispatch task — handler returns immediately
///    a. Augment message with context preamble
///    b. Dispatch to agent
///    c. Record exchange, send reply
fn handle_message_nonblocking(
    bot: Bot,
    msg: Message,
    config: Arc<CalciforgeConfig>,
    router: Arc<Router>,
    command_handler: Arc<CommandHandler>,
    context_store: ContextStore,
    choice_state: Arc<ChoiceState>,
) {
    let received_at = std::time::Instant::now();
    let chat_id = msg.chat.id;
    let delivery_lag_ms = telemetry::delivery_lag_ms_from_unix_seconds(msg.date.timestamp() as u64);

    // Extract text (ignore non-text messages like photos, stickers, etc.)
    let text = match msg.text() {
        Some(t) => t.to_string(),
        None => {
            debug!(chat_id = %chat_id, "ignoring non-text message");
            return;
        }
    };

    // Extract sender user ID — needed for auth and context labels.
    let user = match msg.from.as_ref() {
        Some(u) => u,
        None => {
            debug!(chat_id = %chat_id, "message has no sender, dropping");
            return;
        }
    };
    let sender_id = user.id.0 as i64;

    // Auth boundary: resolve sender to identity.
    // Synchronous — no await, so we can branch immediately on the result.
    let identity = match resolve_telegram_sender(sender_id, &config) {
        Some(id) => id,
        None => {
            warn!(sender_id = %sender_id, "unknown Telegram sender — dropping silently");
            return;
        }
    };

    telemetry::authorized_message(
        "telegram",
        &identity.id,
        &sender_id.to_string(),
        text.len(),
        delivery_lag_ms,
    );

    // Context key: scoped to (chat_id, identity_id) so each identity has isolated
    // conversation history even within the same Telegram chat.
    let chat_key = format!("{}-{}", chat_id.0, identity.id);

    // ── Pending-choice resolution (free-text) ────────────────────────────
    match choice_state.match_reply("telegram", &identity.id, &text) {
        ChoiceMatchResult::Match { command, label, .. } => {
            info!(
                identity = %identity.id,
                label = %label,
                "Telegram: text reply matched pending choice"
            );
            let cmd_handler = command_handler.clone();
            let iid = identity.id.clone();
            let bot2 = bot.clone();
            tokio::spawn(async move {
                let reply = if CommandHandler::is_switch_command(&command) {
                    cmd_handler.handle_switch(&command, &iid)
                } else if CommandHandler::is_model_command(&command) {
                    cmd_handler.handle_model(&command, &iid)
                } else if let Some(r) = cmd_handler.handle(&command) {
                    r
                } else {
                    format!("Selected: {label}")
                };
                send_plain_reply(bot2, chat_id, reply, "choice_match").await;
            });
            return;
        }
        ChoiceMatchResult::Ambiguous => {
            let bot2 = bot.clone();
            tokio::spawn(async move {
                send_plain_reply(
                    bot2,
                    chat_id,
                    "Multiple options match. Reply with the number, or be more specific."
                        .to_string(),
                    "choice_ambiguous",
                )
                .await;
            });
            return;
        }
        ChoiceMatchResult::OutOfRange => {
            let bot2 = bot.clone();
            tokio::spawn(async move {
                send_plain_reply(
                    bot2,
                    chat_id,
                    "That number isn't one of the options. Reply with a number from the list, or the option name.".to_string(),
                    "choice_out_of_range",
                )
                .await;
            });
            return;
        }
        _ => {}
    }

    // -----------------------------------------------------------------------
    // Command fast-path — all handled synchronously, reply spawned immediately.
    // These return before the handler, keeping the Teloxide dispatcher free.
    // -----------------------------------------------------------------------

    if CommandHandler::is_agent_choice_request(&text) {
        let rich_ui = channel_allows_rich_ui(&config, "telegram");
        debug!(chat_id = %chat_id, identity = %identity.id, "showing agent choice buttons");
        let reply = command_handler
            .agent_choice_message_for_identity(&text, &identity.id)
            .unwrap_or_else(|| OutboundMessage::text("Configured agents unavailable."));
        if !reply.controls.is_empty() {
            choice_state.record("telegram", &identity.id, reply.controls.clone());
        }
        telemetry::command_reply_ready(
            "telegram",
            &identity.id,
            "agent_choices",
            received_at.elapsed().as_millis() as u64,
            0,
            reply.response_len(),
        );
        let bot2 = bot.clone();
        tokio::spawn(async move {
            send_choice_reply(bot2, chat_id, reply, rich_ui, "agent_choices").await;
        });
        return;
    }

    if CommandHandler::is_model_choice_request(&text) {
        let rich_ui = channel_allows_rich_ui(&config, "telegram");
        debug!(chat_id = %chat_id, identity = %identity.id, "showing model choice buttons");
        let reply = command_handler
            .model_choice_message(&text)
            .unwrap_or_else(|| {
                OutboundMessage::text("No activatable model choices are configured.")
            });
        if !reply.controls.is_empty() {
            choice_state.record("telegram", &identity.id, reply.controls.clone());
        }
        telemetry::command_reply_ready(
            "telegram",
            &identity.id,
            "model_choices",
            received_at.elapsed().as_millis() as u64,
            0,
            reply.response_len(),
        );
        let bot2 = bot.clone();
        tokio::spawn(async move {
            send_choice_reply(bot2, chat_id, reply, rich_ui, "model_choices").await;
        });
        return;
    }

    // Pre-auth-safe commands — no identity context needed.
    if let Some(reply) = command_handler.handle(&text) {
        debug!(chat_id = %chat_id, cmd = %text.trim(), "handled local pre-auth command");
        telemetry::command_reply_ready(
            "telegram",
            &identity.id,
            "command",
            received_at.elapsed().as_millis() as u64,
            0,
            reply.len(),
        );
        let bot2 = bot.clone();
        tokio::spawn(async move {
            send_plain_reply(bot2, chat_id, reply, "command").await;
        });
        return;
    }

    // If the text looks like a !command but wasn't handled as a pre-auth
    // local command and it is NOT a post-auth command (status/switch/default/sessions/secure),
    // reply with a helpful unknown-command message rather than routing it to an agent.
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
        let reply = command_handler.unknown_command(&text);
        telemetry::command_reply_ready(
            "telegram",
            &identity.id,
            "unknown_command",
            received_at.elapsed().as_millis() as u64,
            0,
            reply.len(),
        );
        let bot2 = bot.clone();
        tokio::spawn(async move {
            send_plain_reply(bot2, chat_id, reply, "unknown_command").await;
        });
        return;
    }

    // !status — requires identity context; handled post-auth so it shows the
    // per-identity active agent (respects !switch overrides).
    if CommandHandler::is_status_command(&text) {
        debug!(chat_id = %chat_id, identity = %identity.id, "handling !status command");
        let bot2 = bot.clone();
        let identity_id = identity.id.clone();
        let command_handler2 = command_handler.clone();
        tokio::spawn(async move {
            let command_start = std::time::Instant::now();
            let reply = command_handler2.cmd_status_for_identity(&identity_id).await;
            telemetry::command_reply_ready(
                "telegram",
                &identity_id,
                "status",
                received_at.elapsed().as_millis() as u64,
                command_start.elapsed().as_millis() as u64,
                reply.len(),
            );
            send_plain_reply(bot2, chat_id, reply, "status").await;
        });
        return;
    }

    // !switch — requires identity context; handled post-auth.
    if CommandHandler::is_switch_command(&text) {
        debug!(chat_id = %chat_id, identity = %identity.id, "handling !switch command");
        let command_start = std::time::Instant::now();
        let reply = command_handler.handle_switch(&text, &identity.id);
        telemetry::command_reply_ready(
            "telegram",
            &identity.id,
            "switch",
            received_at.elapsed().as_millis() as u64,
            command_start.elapsed().as_millis() as u64,
            reply.len(),
        );
        let bot2 = bot.clone();
        tokio::spawn(async move {
            send_plain_reply(bot2, chat_id, reply, "switch").await;
        });
        return;
    }

    // !model — requires identity context for alloy selection; handled post-auth.
    if CommandHandler::is_model_command(&text) {
        debug!(chat_id = %chat_id, identity = %identity.id, "handling !model command");
        let command_start = std::time::Instant::now();
        let reply = command_handler.handle_model(&text, &identity.id);
        telemetry::command_reply_ready(
            "telegram",
            &identity.id,
            "model",
            received_at.elapsed().as_millis() as u64,
            command_start.elapsed().as_millis() as u64,
            reply.len(),
        );
        let bot2 = bot.clone();
        tokio::spawn(async move {
            send_plain_reply(bot2, chat_id, reply, "model").await;
        });
        return;
    }

    // !sessions — list ACP sessions for an agent; requires identity context.
    if CommandHandler::is_sessions_command(&text) {
        debug!(chat_id = %chat_id, identity = %identity.id, "handling !sessions command");
        let bot2 = bot.clone();
        let identity_id = identity.id.clone();
        let command_handler2 = command_handler.clone();
        let cs = choice_state.clone();
        tokio::spawn(async move {
            let command_start = std::time::Instant::now();
            let reply = command_handler2
                .handle_sessions_message(&text, &identity_id)
                .await;
            if !reply.controls.is_empty() {
                cs.record("telegram", &identity_id, reply.controls.clone());
            }
            telemetry::command_reply_ready(
                "telegram",
                &identity_id,
                "sessions",
                received_at.elapsed().as_millis() as u64,
                command_start.elapsed().as_millis() as u64,
                reply.response_len(),
            );
            send_choice_reply(
                bot2,
                chat_id,
                reply,
                channel_allows_rich_ui(&config, "telegram"),
                "sessions",
            )
            .await;
        });
        return;
    }

    // !default — switch back to configured default agent; requires identity context.
    if CommandHandler::is_default_command(&text) {
        debug!(chat_id = %chat_id, identity = %identity.id, "handling !default command");
        let command_start = std::time::Instant::now();
        let reply = command_handler.handle_default(&identity.id);
        telemetry::command_reply_ready(
            "telegram",
            &identity.id,
            "default",
            received_at.elapsed().as_millis() as u64,
            command_start.elapsed().as_millis() as u64,
            reply.len(),
        );
        let bot2 = bot.clone();
        tokio::spawn(async move {
            send_plain_reply(bot2, chat_id, reply, "default").await;
        });
        return;
    }

    // !secret / !secure — store/list secrets without ever routing the value to an
    // agent. Runs post-auth so we can audit who set what; doesn't yet
    // gate by role (open for any authenticated identity). The handler
    // is async because it shells out to `fnox`.
    //
    // We deliberately do NOT log `text` here or in the handler — the
    // chat `set` form contains a secret value that must
    // not appear in ops logs. debug! logs only that a secret command
    // was handled; the handler never logs `text` either.
    if CommandHandler::is_secure_command(&text) {
        debug!(chat_id = %chat_id, identity = %identity.id, "handling secret command");
        if CommandHandler::is_secure_set_command(&text)
            && !crate::config::channel_allows_chat_secret_set(&config, "telegram")
        {
            let reply = CommandHandler::secure_set_disabled_reply("Telegram");
            telemetry::command_reply_ready(
                "telegram",
                &identity.id,
                "secure_disabled",
                received_at.elapsed().as_millis() as u64,
                0,
                reply.len(),
            );
            let bot2 = bot.clone();
            tokio::spawn(async move {
                send_secret_reply(
                    bot2,
                    chat_id,
                    reply,
                    "secure_disabled",
                    channel_allows_rich_ui(&config, "telegram"),
                )
                .await;
            });
            return;
        }
        let cmd_handler = command_handler.clone();
        let identity_id = identity.id.clone();
        let bot2 = bot.clone();
        let text_for_handler = text.clone();
        tokio::spawn(async move {
            let command_start = std::time::Instant::now();
            let reply = cmd_handler
                .handle_secure(&text_for_handler, &identity_id)
                .await;
            telemetry::command_reply_ready(
                "telegram",
                &identity_id,
                "secure",
                received_at.elapsed().as_millis() as u64,
                command_start.elapsed().as_millis() as u64,
                reply.len(),
            );
            send_secret_reply(
                bot2,
                chat_id,
                reply,
                "secure",
                channel_allows_rich_ui(&config, "telegram"),
            )
            .await;
        });
        return;
    }

    // !context clear — clear the conversation buffer for this chat.
    if text.trim().eq_ignore_ascii_case("!context clear") {
        context_store.clear(&chat_key);
        telemetry::command_reply_ready(
            "telegram",
            &identity.id,
            "context_clear",
            received_at.elapsed().as_millis() as u64,
            0,
            "🧹 Conversation context cleared.".len(),
        );
        let bot2 = bot.clone();
        tokio::spawn(async move {
            send_plain_reply(
                bot2,
                chat_id,
                "🧹 Conversation context cleared.",
                "context_clear",
            )
            .await;
        });
        return;
    }

    // !approve / !deny — async approval commands delegated to CommandHandler.
    if CommandHandler::is_approve_command(&text) || CommandHandler::is_deny_command(&text) {
        debug!(chat_id = %chat_id, identity = %identity.id, cmd = %text.trim(), "handling async approval command");
        let cmd = command_handler.clone();
        let text_owned = text.clone();
        let bot2 = bot.clone();
        tokio::spawn(async move {
            let command_start = std::time::Instant::now();
            if let Some((ack, follow_up)) = cmd.handle_async(&text_owned).await {
                telemetry::command_reply_ready(
                    "telegram",
                    &identity.id,
                    "approval",
                    received_at.elapsed().as_millis() as u64,
                    command_start.elapsed().as_millis() as u64,
                    ack.len() + follow_up.as_ref().map(|s| s.len()).unwrap_or(0),
                );
                send_plain_reply(bot2.clone(), chat_id, ack, "approval_ack").await;
                if let Some(resp) = follow_up {
                    send_markdown_reply(bot2, chat_id, resp, "approval_follow_up").await;
                }
            }
        });
        return;
    }

    // -----------------------------------------------------------------------
    // Agent dispatch — resolve synchronously, then spawn.
    // -----------------------------------------------------------------------

    // Resolve active agent for this identity (respects !switch overrides).
    let agent_id = match command_handler.active_agent_for(&identity.id) {
        Some(id) => id,
        None => {
            warn!(identity = %identity.id, "no routing rule for identity — dropping");
            return;
        }
    };

    let agent = match find_agent(&agent_id, &config) {
        Some(a) => a.clone(),
        None => {
            warn!(agent_id = %agent_id, "agent not found in config");
            let bot2 = bot.clone();
            tokio::spawn(async move {
                send_plain_reply(
                    bot2,
                    chat_id,
                    "⚠️ Agent not configured.",
                    "agent_not_configured",
                )
                .await;
            });
            return;
        }
    };

    // Resolve a human-readable sender label for context preambles.
    // Prefer display_name from identity config, fall back to identity id.
    let sender_label = config
        .identities
        .iter()
        .find(|i| i.id == identity.id)
        .and_then(|i| i.display_name.as_deref())
        .unwrap_or(&identity.id)
        .to_string();
    let model_override = command_handler.active_model_for_identity(&identity.id);
    let selected_session = command_handler.active_session_for(&identity.id, &agent_id);
    let preserve_native_commands = crate::adapters::agent_supports_native_commands(&agent);

    // Spawn the agent dispatch — handler returns immediately.
    // All slow I/O (context augment, HTTP, reply send) happens in the task.
    tokio::spawn(async move {
        let queue_wait_ms = received_at.elapsed().as_millis() as u64;
        telemetry::agent_dispatch_started("telegram", &identity.id, &agent_id, queue_wait_ms);
        // Augment message with conversation context (unseen exchanges for this agent).
        let augmented_text = context_store.augment_message_with_options(
            &chat_key,
            &agent_id,
            &text,
            preserve_native_commands,
        );

        if augmented_text.len() > text.len() {
            debug!(
                chat_id = %chat_id,
                identity = %identity.id,
                agent_id = %agent_id,
                original_len = %text.len(),
                augmented_len = %augmented_text.len(),
                "injected conversation context preamble"
            );
        }

        let dispatch_start = std::time::Instant::now();
        match router
            .dispatch_message_with_full_context(
                &augmented_text,
                &agent,
                &config,
                crate::router::RouterDispatchContext {
                    sender: Some(&identity.id),
                    model_override: model_override.as_deref(),
                    session: selected_session.as_deref(),
                    channel: Some("telegram"),
                },
            )
            .await
        {
            Ok(response_message) => {
                if !response_message.controls.is_empty() {
                    choice_state.record(
                        "telegram",
                        &identity.id,
                        response_message.controls.clone(),
                    );
                }
                let response = response_message.render_text_fallback();
                let latency_ms = dispatch_start.elapsed().as_millis() as u64;
                command_handler.record_dispatch(latency_ms);
                telemetry::agent_dispatch_succeeded(
                    "telegram",
                    &identity.id,
                    &agent_id,
                    latency_ms,
                    response_message.response_len(),
                );
                debug!(
                    identity = %identity.id,
                    agent_id = %agent_id,
                    response_len = %response_message.response_len(),
                    attachments = response_message.attachments.len(),
                    "got agent response"
                );

                // Record the exchange (original, un-augmented prompt) in the context buffer.
                context_store.push_with_options(
                    &chat_key,
                    &sender_label,
                    &text,
                    &agent_id,
                    &response,
                    preserve_native_commands,
                );

                send_outbound_reply(bot, chat_id, response_message, "agent_response").await;
            }
            Err(e) => {
                // ── Clash approval flow ─────────────────────────────────────
                // Check if the agent loop paused for human approval.
                if let Some(crate::adapters::AdapterError::ApprovalPending(req)) =
                    e.downcast_ref::<crate::adapters::AdapterError>()
                {
                    let req = req.clone();
                    debug!(
                        request_id = %req.request_id,
                        command = %req.command,
                        "clash: forwarding approval request to user"
                    );
                    // Register in command handler so !approve / !deny can find it.
                    command_handler
                        .register_pending_approval(crate::adapters::openclaw::PendingApprovalMeta {
                            request_id: req.request_id.clone(),
                            zeroclaw_endpoint: agent.endpoint.clone(),
                            zeroclaw_auth_token: agent.auth_token.clone().unwrap_or_default(),
                            _summary: CommandHandler::approval_request_message(
                                &req.command,
                                &req.reason,
                                &req.request_id,
                            )
                            .render_text_fallback(),
                        })
                        .await;

                    // Send the approval notification to the user.
                    let notification = CommandHandler::approval_request_message(
                        &req.command,
                        &req.reason,
                        &req.request_id,
                    );
                    if !notification.controls.is_empty() {
                        choice_state.record(
                            "telegram",
                            &identity.id,
                            notification.controls.clone(),
                        );
                    }
                    send_choice_reply(
                        bot,
                        chat_id,
                        notification,
                        channel_allows_rich_ui(&config, "telegram"),
                        "approval_request",
                    )
                    .await;
                    return; // Don't send an error — we already notified.
                }
                // ─────────────────────────────────────────────────────────────
                warn!(identity = %identity.id, error = %e, "agent dispatch failed");
                send_plain_reply(
                    bot,
                    chat_id,
                    format!("⚠️ Agent error: {}", e),
                    "agent_error",
                )
                .await;
            }
        }
    });
}

async fn send_outbound_reply(
    bot: Bot,
    chat_id: ChatId,
    reply: OutboundMessage,
    reply_kind: &'static str,
) {
    if reply.attachments.is_empty() {
        send_markdown_reply(bot, chat_id, reply.render_text_fallback(), reply_kind).await;
        return;
    }

    if let Some(text) = reply.text.as_deref().filter(|text| !text.trim().is_empty()) {
        send_markdown_reply(bot.clone(), chat_id, text.to_string(), reply_kind).await;
    }

    for attachment in &reply.attachments {
        if !send_telegram_attachment(bot.clone(), chat_id, attachment, reply_kind).await {
            warn!(
                chat_id = %chat_id,
                path = %attachment.path.display(),
                "Telegram native artifact send failed; sending text fallback"
            );
            send_plain_reply(
                bot,
                chat_id,
                reply.render_text_fallback(),
                "agent_response_attachment_fallback",
            )
            .await;
            return;
        }
    }
}

async fn send_telegram_attachment(
    bot: Bot,
    chat_id: ChatId,
    attachment: &OutboundAttachment,
    reply_kind: &'static str,
) -> bool {
    const MAX_TELEGRAM_UPLOAD_BYTES: u64 = 25 * 1024 * 1024;

    let start = std::time::Instant::now();
    match tokio::fs::metadata(&attachment.path).await {
        Ok(metadata) if !metadata.is_file() => {
            telemetry::reply_failed(
                "telegram",
                &chat_id.to_string(),
                reply_kind,
                start.elapsed().as_millis() as u64,
                format!("artifact path is not a file: {}", attachment.path.display()),
            );
            return false;
        }
        Ok(metadata) if metadata.len() > MAX_TELEGRAM_UPLOAD_BYTES => {
            telemetry::reply_failed(
                "telegram",
                &chat_id.to_string(),
                reply_kind,
                start.elapsed().as_millis() as u64,
                format!(
                    "artifact is {} bytes, limit is {}",
                    metadata.len(),
                    MAX_TELEGRAM_UPLOAD_BYTES
                ),
            );
            return false;
        }
        Ok(_) => {}
        Err(e) => {
            telemetry::reply_failed(
                "telegram",
                &chat_id.to_string(),
                reply_kind,
                start.elapsed().as_millis() as u64,
                e,
            );
            return false;
        }
    }

    let input = InputFile::file(attachment.path.clone());
    let caption = attachment
        .caption
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    let result = match attachment.kind {
        AttachmentKind::Image => {
            let request = bot.send_photo(chat_id, input);
            if let Some(caption) = caption {
                request.caption(caption.to_string()).await
            } else {
                request.await
            }
        }
        _ => {
            let request = bot.send_document(chat_id, input);
            if let Some(caption) = caption {
                request.caption(caption.to_string()).await
            } else {
                request.await
            }
        }
    };

    match result {
        Ok(_) => {
            telemetry::reply_sent(
                "telegram",
                &chat_id.to_string(),
                reply_kind,
                attachment.size_bytes as usize,
                start.elapsed().as_millis() as u64,
            );
            true
        }
        Err(e) => {
            telemetry::reply_failed(
                "telegram",
                &chat_id.to_string(),
                reply_kind,
                start.elapsed().as_millis() as u64,
                e,
            );
            false
        }
    }
}

async fn send_plain_reply(
    bot: Bot,
    chat_id: ChatId,
    reply: impl Into<String>,
    reply_kind: &'static str,
) {
    let reply = reply.into();
    let response_len = reply.len();
    let start = std::time::Instant::now();
    match bot.send_message(chat_id, reply).await {
        Ok(_) => telemetry::reply_sent(
            "telegram",
            &chat_id.to_string(),
            reply_kind,
            response_len,
            start.elapsed().as_millis() as u64,
        ),
        Err(e) => telemetry::reply_failed(
            "telegram",
            &chat_id.to_string(),
            reply_kind,
            start.elapsed().as_millis() as u64,
            e,
        ),
    }
}

async fn send_choice_reply(
    bot: Bot,
    chat_id: ChatId,
    reply: OutboundMessage,
    rich_ui: bool,
    reply_kind: &'static str,
) {
    let response_len = reply.response_len();
    let keyboard = telegram_keyboard_for_message(&reply, rich_ui);
    let reply = reply.render_text_fallback();
    let start = std::time::Instant::now();
    let mut request = bot.send_message(chat_id, &reply);
    if let Some(keyboard) = keyboard {
        request = request.reply_markup(keyboard);
    }

    match request.await {
        Ok(_) => telemetry::reply_sent(
            "telegram",
            &chat_id.to_string(),
            reply_kind,
            response_len,
            start.elapsed().as_millis() as u64,
        ),
        Err(e) => telemetry::reply_failed(
            "telegram",
            &chat_id.to_string(),
            reply_kind,
            start.elapsed().as_millis() as u64,
            e,
        ),
    }
}

async fn send_secret_reply(
    bot: Bot,
    chat_id: ChatId,
    reply: impl Into<String>,
    reply_kind: &'static str,
    rich_ui: bool,
) {
    let reply = reply.into();
    let response_len = reply.len();
    let start = std::time::Instant::now();
    let mut request = bot.send_message(chat_id, &reply);
    if rich_ui {
        if let Some(url) = first_http_url(&reply) {
            let keyboard = InlineKeyboardMarkup::default()
                .append_row(vec![InlineKeyboardButton::url("Open paste form", url)]);
            request = request.reply_markup(keyboard);
        }
    }

    match request.await {
        Ok(_) => telemetry::reply_sent(
            "telegram",
            &chat_id.to_string(),
            reply_kind,
            response_len,
            start.elapsed().as_millis() as u64,
        ),
        Err(e) => telemetry::reply_failed(
            "telegram",
            &chat_id.to_string(),
            reply_kind,
            start.elapsed().as_millis() as u64,
            e,
        ),
    }
}

async fn handle_callback_query(
    bot: Bot,
    query: CallbackQuery,
    config: Arc<CalciforgeConfig>,
    command_handler: Arc<CommandHandler>,
) {
    let sender_id = query.from.id.0 as i64;
    let identity = match resolve_telegram_sender(sender_id, &config) {
        Some(identity) => identity,
        None => {
            warn!(sender_id = %sender_id, "unknown Telegram callback sender — dropping");
            let _ = bot
                .answer_callback_query(query.id)
                .text("This Telegram user is not allowed.")
                .await;
            return;
        }
    };

    if !channel_allows_rich_ui(&config, "telegram") {
        let _ = bot
            .answer_callback_query(query.id)
            .text("Buttons are disabled for this channel.")
            .await;
        return;
    }

    let data = query.data.as_deref().unwrap_or_default();
    let Some(action) = parse_callback_action(data) else {
        let _ = bot
            .answer_callback_query(query.id)
            .text("Unknown Calciforge action.")
            .await;
        return;
    };

    if let TelegramCallbackAction::Approve(request_id) | TelegramCallbackAction::Deny(request_id) =
        action
    {
        let chat_id = query.message.as_ref().map(|message| message.chat().id);
        let _ = bot
            .answer_callback_query(query.id)
            .text("Approval submitted.")
            .await;
        let Some(chat_id) = chat_id else {
            return;
        };
        let bot2 = bot.clone();
        let command_handler2 = command_handler.clone();
        let request_id = request_id.to_string();
        let approve = matches!(action, TelegramCallbackAction::Approve(_));
        tokio::spawn(async move {
            let command = if approve {
                format!("!approve {request_id}")
            } else {
                format!("!deny {request_id}")
            };
            let (ack, follow_up) = if approve {
                command_handler2.handle_approve(&command).await
            } else {
                command_handler2.handle_deny(&command).await
            };
            send_plain_reply(bot2.clone(), chat_id, ack, "callback_action").await;
            if let Some(resp) = follow_up {
                send_markdown_reply(bot2, chat_id, resp, "approval_follow_up").await;
            }
        });
        return;
    }

    let reply = match action {
        TelegramCallbackAction::Agent(agent_id) => {
            command_handler.handle_switch(&format!("!agent switch {agent_id}"), &identity.id)
        }
        TelegramCallbackAction::Model(model_id) => {
            command_handler.handle_model(&format!("!model use {model_id}"), &identity.id)
        }
        TelegramCallbackAction::Session { agent_id, session } => {
            command_handler.handle_switch(&format!("!switch {agent_id} {session}"), &identity.id)
        }
        TelegramCallbackAction::Approve(_) | TelegramCallbackAction::Deny(_) => unreachable!(),
    };

    let chat_id = query.message.as_ref().map(|message| message.chat().id);
    let feedback = callback_feedback(&reply, chat_id.is_some());
    let mut answer = bot.answer_callback_query(query.id).text(feedback.text);
    if feedback.show_alert {
        answer = answer.show_alert(true);
    }
    let _ = answer.await;

    if let Some(chat_id) = chat_id {
        send_plain_reply(bot, chat_id, reply, "callback_action").await;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TelegramCallbackAction<'a> {
    Agent(&'a str),
    Model(&'a str),
    Session { agent_id: &'a str, session: &'a str },
    Approve(&'a str),
    Deny(&'a str),
}

fn parse_callback_action(data: &str) -> Option<TelegramCallbackAction<'_>> {
    let mut parts = data.splitn(4, ':');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("cf"), Some("agent"), Some(agent_id), None) if !agent_id.trim().is_empty() => {
            Some(TelegramCallbackAction::Agent(agent_id))
        }
        (Some("cf"), Some("model"), Some(model_id), None) if !model_id.trim().is_empty() => {
            Some(TelegramCallbackAction::Model(model_id))
        }
        (Some("cf"), Some("session"), Some(agent_id), Some(session))
            if !agent_id.trim().is_empty() && !session.trim().is_empty() =>
        {
            Some(TelegramCallbackAction::Session { agent_id, session })
        }
        (Some("cf"), Some("approve"), Some(request_id), None) if !request_id.trim().is_empty() => {
            Some(TelegramCallbackAction::Approve(request_id))
        }
        (Some("cf"), Some("deny"), Some(request_id), None) if !request_id.trim().is_empty() => {
            Some(TelegramCallbackAction::Deny(request_id))
        }
        _ => None,
    }
}

fn telegram_keyboard_for_message(
    message: &OutboundMessage,
    rich_ui: bool,
) -> Option<InlineKeyboardMarkup> {
    rich_ui
        .then(|| choice_keyboard_from_controls(message))
        .flatten()
}

fn choice_keyboard_from_controls(message: &OutboundMessage) -> Option<InlineKeyboardMarkup> {
    const MAX_TELEGRAM_CALLBACK_BYTES: usize = 64;
    const MAX_TELEGRAM_BUTTON_TEXT_CHARS: usize = 64;
    let mut rows = Vec::new();
    let mut row = Vec::new();
    for (option, data) in message
        .controls
        .iter()
        .flat_map(|control| control.options.iter())
        .filter_map(|option| {
            let data = option.callback_data.as_deref()?;
            (data.len() <= MAX_TELEGRAM_CALLBACK_BYTES).then_some((option, data))
        })
        .take(20)
    {
        row.push(InlineKeyboardButton::callback(
            telegram_button_text(&option.label, MAX_TELEGRAM_BUTTON_TEXT_CHARS),
            data.to_string(),
        ));
        if row.len() == 2 {
            rows.push(std::mem::take(&mut row));
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    if rows.is_empty() {
        None
    } else {
        Some(InlineKeyboardMarkup::new(rows))
    }
}

fn telegram_button_text(label: &str, max_chars: usize) -> String {
    if label.chars().count() <= max_chars {
        return label.to_string();
    }
    let keep = max_chars.saturating_sub(3);
    let mut text = label.chars().take(keep).collect::<String>();
    text.push_str("...");
    text
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CallbackFeedback {
    text: String,
    show_alert: bool,
}

fn callback_feedback(reply: &str, chat_will_receive_reply: bool) -> CallbackFeedback {
    let success = callback_reply_succeeded(reply);
    if chat_will_receive_reply {
        return CallbackFeedback {
            text: if success {
                "Updated.".to_string()
            } else {
                "Action was not applied.".to_string()
            },
            show_alert: !success,
        };
    }

    CallbackFeedback {
        text: truncate_callback_text(reply.trim()),
        show_alert: !success,
    }
}

fn callback_reply_succeeded(reply: &str) -> bool {
    let reply = reply.trim_start();
    reply.starts_with('✅') || reply.starts_with('🔄')
}

fn truncate_callback_text(text: &str) -> String {
    const MAX_CALLBACK_ANSWER_CHARS: usize = 200;
    let char_count = text.chars().count();
    if char_count <= MAX_CALLBACK_ANSWER_CHARS {
        return text.to_string();
    }
    let mut truncated = text
        .chars()
        .take(MAX_CALLBACK_ANSWER_CHARS.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
}

fn first_http_url(text: &str) -> Option<reqwest::Url> {
    text.split_whitespace().find_map(|token| {
        let candidate = token.trim_matches(|c: char| {
            matches!(c, '<' | '>' | '(' | ')' | '[' | ']' | ',' | '.' | '"')
        });
        reqwest::Url::parse(candidate).ok().filter(|url| {
            let scheme = url.scheme();
            scheme == "http" || scheme == "https"
        })
    })
}

async fn send_markdown_reply(
    bot: Bot,
    chat_id: ChatId,
    reply: impl Into<String>,
    reply_kind: &'static str,
) {
    let reply = reply.into();
    let response_len = reply.len();
    let start = std::time::Instant::now();
    let send_result = bot
        .send_message(chat_id, &reply)
        .parse_mode(ParseMode::MarkdownV2)
        .await;
    match send_result {
        Ok(_) => telemetry::reply_sent(
            "telegram",
            &chat_id.to_string(),
            reply_kind,
            response_len,
            start.elapsed().as_millis() as u64,
        ),
        Err(markdown_error) => {
            debug!(
                chat_id = %chat_id,
                reply_kind,
                error = %markdown_error,
                "Telegram MarkdownV2 send failed; retrying as plain text"
            );
            send_plain_reply(bot, chat_id, reply, reply_kind).await;
        }
    }
}

/// Handle a single incoming Telegram message (async, awaits agent response).
///
/// **Deprecated in favour of [`handle_message_nonblocking`]** which spawns agent
/// dispatch so commands remain responsive.  Kept for reference / testing.
///
/// Message flow:
/// 1. Extract text + sender
/// 2. Auth — unknown sender → drop silently
/// 3. Build per-identity context key `"{chat_id}-{identity_id}"` (isolates context per identity)
/// 4. Pre-auth commands (`!ping`, `!help`, etc.) — reply and return
/// 5. `!switch <agent>` — handle with identity context, reply and return
/// 6. Resolve active agent for this identity
/// 7. Augment message with conversation context preamble (unseen exchanges)
/// 8. Dispatch to agent
/// 9. Record exchange in context buffer, reply to user
#[allow(dead_code)]
async fn handle_message(
    bot: Bot,
    msg: Message,
    config: Arc<CalciforgeConfig>,
    router: Arc<Router>,
    command_handler: Arc<CommandHandler>,
    context_store: ContextStore,
) {
    let chat_id = msg.chat.id;

    // Extract text (ignore non-text messages like photos, stickers, etc.)
    let text = match msg.text() {
        Some(t) => t.to_string(),
        None => {
            debug!(chat_id = %chat_id, "ignoring non-text message");
            return;
        }
    };

    // Extract sender user ID — needed for auth and context labels.
    let user = match msg.from.as_ref() {
        Some(u) => u,
        None => {
            debug!(chat_id = %chat_id, "message has no sender, dropping");
            return;
        }
    };
    let sender_id = user.id.0 as i64;

    // Auth boundary: resolve sender to identity.
    // Must be synchronous (no await) so identity is available for all subsequent
    // command checks without any async race.
    let identity = match resolve_telegram_sender(sender_id, &config) {
        Some(id) => id,
        None => {
            warn!(sender_id = %sender_id, "unknown Telegram sender — dropping silently");
            return;
        }
    };

    info!(
        identity = %identity.id,
        sender_id = %sender_id,
        text_len = %text.len(),
        "authorized message from identity"
    );

    // Context key: scoped to (chat_id, identity_id) so each identity has isolated
    // conversation history even within the same Telegram chat.
    // This prevents context bleed when an operator switches between identities.
    let chat_key = format!("{}-{}", chat_id.0, identity.id);

    // Pre-auth-safe commands — no identity context needed, intercept before any await.
    if let Some(reply) = command_handler.handle(&text) {
        debug!(chat_id = %chat_id, cmd = %text.trim(), "handled local pre-auth command");
        if let Err(e) = bot.send_message(chat_id, &reply).await {
            warn!(chat_id = %chat_id, error = %e, "failed to send command reply");
        }
        return;
    }

    // !status — requires identity context; handled post-auth so it shows the
    // per-identity active agent (respects !switch overrides).
    if CommandHandler::is_status_command(&text) {
        debug!(chat_id = %chat_id, identity = %identity.id, "handling !status command");
        let reply = command_handler.cmd_status_for_identity(&identity.id).await;
        if let Err(e) = bot.send_message(chat_id, &reply).await {
            warn!(chat_id = %chat_id, error = %e, "failed to send status reply");
        }
        return;
    }

    // !switch — requires identity context; handled post-auth.
    if CommandHandler::is_switch_command(&text) {
        debug!(chat_id = %chat_id, identity = %identity.id, "handling !switch command");
        let reply = command_handler.handle_switch(&text, &identity.id);
        if let Err(e) = bot.send_message(chat_id, &reply).await {
            warn!(chat_id = %chat_id, error = %e, "failed to send switch reply");
        }
        return;
    }

    // !model — requires identity context for alloy selection; handled post-auth.
    if CommandHandler::is_model_command(&text) {
        debug!(chat_id = %chat_id, identity = %identity.id, "handling !model command");
        let reply = command_handler.handle_model(&text, &identity.id);
        if let Err(e) = bot.send_message(chat_id, &reply).await {
            warn!(chat_id = %chat_id, error = %e, "failed to send model reply");
        }
        return;
    }

    // !sessions — list ACP sessions for an agent; requires identity context.
    if CommandHandler::is_sessions_command(&text) {
        debug!(chat_id = %chat_id, identity = %identity.id, "handling !sessions command");
        let reply = command_handler
            .handle_sessions_message(&text, &identity.id)
            .await;
        let rendered = reply.render_text_fallback();
        if let Err(e) = bot.send_message(chat_id, &rendered).await {
            warn!(chat_id = %chat_id, error = %e, "failed to send sessions reply");
        }
        return;
    }

    // !default — switch back to configured default agent; requires identity context.
    if CommandHandler::is_default_command(&text) {
        debug!(chat_id = %chat_id, identity = %identity.id, "handling !default command");
        let reply = command_handler.handle_default(&identity.id);
        if let Err(e) = bot.send_message(chat_id, &reply).await {
            warn!(chat_id = %chat_id, error = %e, "failed to send default reply");
        }
        return;
    }

    // !secret / !secure — store/list secrets without routing the value to an
    // agent. Logged debug-level with no text to keep the value out of
    // ops logs (the chat `set` form would otherwise be visible).
    if CommandHandler::is_secure_command(&text) {
        debug!(chat_id = %chat_id, identity = %identity.id, "handling secret command");
        if CommandHandler::is_secure_set_command(&text)
            && !crate::config::channel_allows_chat_secret_set(&config, "telegram")
        {
            let reply = CommandHandler::secure_set_disabled_reply("Telegram");
            send_secret_reply(
                bot,
                chat_id,
                reply,
                "secure_disabled",
                channel_allows_rich_ui(&config, "telegram"),
            )
            .await;
            return;
        }
        let reply = command_handler.handle_secure(&text, &identity.id).await;
        send_secret_reply(
            bot,
            chat_id,
            reply,
            "secure",
            channel_allows_rich_ui(&config, "telegram"),
        )
        .await;
        return;
    }

    // !context clear — clear the conversation buffer for this chat.
    if text.trim().eq_ignore_ascii_case("!context clear") {
        context_store.clear(&chat_key);
        if let Err(e) = bot
            .send_message(chat_id, "🧹 Conversation context cleared.")
            .await
        {
            warn!(chat_id = %chat_id, error = %e, "failed to send context-clear reply");
        }
        return;
    }

    // Resolve active agent for this identity (respects !switch overrides).
    let agent_id = match command_handler.active_agent_for(&identity.id) {
        Some(id) => id,
        None => {
            warn!(identity = %identity.id, "no routing rule for identity — dropping");
            return;
        }
    };

    let agent = match find_agent(&agent_id, &config) {
        Some(a) => a.clone(),
        None => {
            warn!(agent_id = %agent_id, "agent not found in config");
            let _ = bot.send_message(chat_id, "⚠️ Agent not configured.").await;
            return;
        }
    };

    // Resolve a human-readable sender label for context preambles.
    // Prefer display_name from identity config, fall back to identity id.
    let sender_label = config
        .identities
        .iter()
        .find(|i| i.id == identity.id)
        .and_then(|i| i.display_name.as_deref())
        .unwrap_or(&identity.id)
        .to_string();
    let model_override = command_handler.active_model_for_identity(&identity.id);
    let selected_session = command_handler.active_session_for(&identity.id, &agent_id);
    let preserve_native_commands = crate::adapters::agent_supports_native_commands(&agent);

    // Augment message with conversation context (unseen exchanges for this agent).
    let augmented_text = context_store.augment_message_with_options(
        &chat_key,
        &agent_id,
        &text,
        preserve_native_commands,
    );

    if augmented_text.len() > text.len() {
        debug!(
            chat_id = %chat_id,
            identity = %identity.id,
            agent_id = %agent_id,
            original_len = %text.len(),
            augmented_len = %augmented_text.len(),
            "injected conversation context preamble"
        );
    }

    // Dispatch to agent
    let dispatch_start = std::time::Instant::now();
    match router
        .dispatch_message_with_full_context(
            &augmented_text,
            &agent,
            &config,
            crate::router::RouterDispatchContext {
                sender: Some(&identity.id),
                model_override: model_override.as_deref(),
                session: selected_session.as_deref(),
                channel: Some("telegram"),
            },
        )
        .await
    {
        Ok(response_message) => {
            let response = response_message.render_text_fallback();
            let latency_ms = dispatch_start.elapsed().as_millis() as u64;
            command_handler.record_dispatch(latency_ms);
            debug!(
                identity = %identity.id,
                agent_id = %agent_id,
                response_len = %response.len(),
                "got agent response"
            );

            // Record the exchange (original, un-augmented prompt) in the context buffer.
            context_store.push_with_options(
                &chat_key,
                &sender_label,
                &text,
                &agent_id,
                &response,
                preserve_native_commands,
            );

            send_outbound_reply(bot, chat_id, response_message, "agent_response").await;
        }
        Err(e) => {
            warn!(identity = %identity.id, error = %e, "agent dispatch failed");
            let _ = bot
                .send_message(chat_id, format!("⚠️ Agent error: {}", e))
                .await;
        }
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

    /// Create a CommandHandler backed by a temp state directory so tests are
    /// isolated from the default active-agent state file on disk.
    fn make_handler(config: Arc<CalciforgeConfig>) -> CommandHandler {
        let tmp = tempfile::tempdir().expect("tempdir for telegram test state isolation");
        CommandHandler::with_state_dir(config, tmp.path().to_path_buf())
    }

    fn make_test_config() -> CalciforgeConfig {
        CalciforgeConfig {
            calciforge: CalciforgeHeader { version: 2 },
            identities: vec![Identity {
                id: "brian".to_string(),
                display_name: Some("Brian".to_string()),
                aliases: vec![ChannelAlias {
                    channel: "telegram".to_string(),
                    id: "7000000001".to_string(),
                }],
                role: Some("owner".to_string()),
            }],
            agents: vec![AgentConfig {
                id: "librarian".to_string(),
                kind: "openclaw-channel".to_string(),
                endpoint: "http://10.0.0.20:18789".to_string(),
                timeout_ms: Some(120000),
                model: None,
                auth_token: Some("REPLACE_WITH_AUTH_TOKEN".to_string()),
                api_key: None,
                api_key_file: None,
                openclaw_agent_id: None,
                allow_model_override: None,
                reply_port: None,
                reply_auth_token: None,
                reply_auth_token_file: None,
                command: None,
                args: None,
                env: None,
                registry: None,
                aliases: vec![],
            }],
            routing: vec![RoutingRule {
                identity: "brian".to_string(),
                default_agent: "librarian".to_string(),
                allowed_agents: vec![],
            }],
            channels: vec![ChannelConfig {
                kind: "telegram".to_string(),
                bot_token_file: Some("~/.config/calciforge/secrets/telegram-token".to_string()),
                enabled: true,
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
        }
    }

    #[test]
    fn test_auth_resolves_known_sender() {
        let config = make_test_config();
        let id = resolve_telegram_sender(7000000001, &config);
        assert!(id.is_some());
        assert_eq!(id.unwrap().id, "brian");
    }

    #[test]
    fn test_auth_drops_unknown_sender() {
        let config = make_test_config();
        let id = resolve_telegram_sender(9999999, &config);
        assert!(id.is_none(), "unknown sender must be dropped");
    }

    #[test]
    fn test_routing_uses_active_agent_not_just_default() {
        let config = Arc::new(make_test_config());
        let handler = make_handler(config.clone());
        assert_eq!(
            handler.active_agent_for("brian"),
            Some("librarian".to_string())
        );
        handler.handle_switch("!switch librarian", "brian");
        assert_eq!(
            handler.active_agent_for("brian"),
            Some("librarian".to_string())
        );
    }

    #[test]
    fn test_find_agent_in_config() {
        let config = make_test_config();
        let agent = find_agent("librarian", &config);
        assert!(agent.is_some());
        assert_eq!(agent.unwrap().endpoint, "http://10.0.0.20:18789");
    }

    #[test]
    fn test_no_routing_for_unknown_identity() {
        let config = Arc::new(make_test_config());
        let handler = make_handler(config);
        assert!(handler.active_agent_for("stranger").is_none());
    }

    // --- context store wiring smoke tests (no live bot) ---

    #[test]
    fn test_context_store_augment_passthrough_on_empty() {
        let store = ContextStore::new(20, 5);
        let out = store.augment_message("chat:1", "librarian", "hello");
        assert_eq!(out, "hello");
    }

    #[test]
    fn test_context_store_push_and_augment() {
        let store = ContextStore::new(20, 5);
        store.push(
            "chat:1",
            "Brian",
            "first question",
            "librarian",
            "first answer",
        );
        // custodian hasn't seen anything
        let out = store.augment_message("chat:1", "custodian", "second question");
        assert!(
            out.starts_with("[Recent context:"),
            "preamble expected: {}",
            out
        );
        assert!(out.ends_with("second question"), "message at end: {}", out);
    }

    #[test]
    fn test_sender_label_resolved_from_display_name() {
        // Integration check: the telegram handler should use "Brian" not "brian"
        // We test the resolution logic directly here since handle_message needs a live bot.
        let config = make_test_config();
        let label = config
            .identities
            .iter()
            .find(|i| i.id == "brian")
            .and_then(|i| i.display_name.as_deref())
            .unwrap_or("brian");
        assert_eq!(label, "Brian");
    }

    #[test]
    fn telegram_choice_requests_accept_legacy_and_noun_commands() {
        assert!(CommandHandler::is_agent_choice_request("!agents"));
        assert!(CommandHandler::is_agent_choice_request("!agent list"));
        assert!(CommandHandler::is_agent_choice_request("!agent\tls"));
        assert!(!CommandHandler::is_agent_choice_request(
            "!agent switch librarian"
        ));

        assert!(CommandHandler::is_model_choice_request("!model"));
        assert!(CommandHandler::is_model_choice_request("!model list"));
        assert!(CommandHandler::is_model_choice_request("!model\tmodels"));
        assert!(!CommandHandler::is_model_choice_request(
            "!model use dispatcher"
        ));
    }

    #[test]
    fn telegram_callback_actions_are_bounded_and_typed() {
        assert_eq!(
            parse_callback_action("cf:agent:librarian"),
            Some(TelegramCallbackAction::Agent("librarian"))
        );
        assert_eq!(
            parse_callback_action("cf:model:dispatcher-test"),
            Some(TelegramCallbackAction::Model("dispatcher-test"))
        );
        assert_eq!(
            parse_callback_action("cf:session:claude-acpx:backend"),
            Some(TelegramCallbackAction::Session {
                agent_id: "claude-acpx",
                session: "backend"
            })
        );
        assert_eq!(
            parse_callback_action("cf:approve:request-1"),
            Some(TelegramCallbackAction::Approve("request-1"))
        );
        assert_eq!(
            parse_callback_action("cf:deny:request-1"),
            Some(TelegramCallbackAction::Deny("request-1"))
        );
        assert_eq!(parse_callback_action("agent:librarian"), None);
        assert_eq!(parse_callback_action("cf:agent:"), None);
    }

    #[test]
    fn telegram_callback_feedback_reflects_success_and_failure() {
        assert_eq!(
            callback_feedback("✅ Switched active agent to Librarian.", true),
            CallbackFeedback {
                text: "Updated.".to_string(),
                show_alert: false,
            }
        );
        assert_eq!(
            callback_feedback("⚠️ Agent 'custodian' is not available to you.", true),
            CallbackFeedback {
                text: "Action was not applied.".to_string(),
                show_alert: true,
            }
        );

        let no_chat = callback_feedback("⚠️ Agent 'custodian' is not available to you.", false);
        assert!(no_chat.text.contains("custodian"), "{no_chat:?}");
        assert!(no_chat.show_alert, "{no_chat:?}");
    }

    #[test]
    fn telegram_callback_feedback_truncates_inline_only_replies() {
        let long_reply = format!("⚠️ {}", "x".repeat(250));
        let feedback = callback_feedback(&long_reply, false);

        assert_eq!(feedback.text.chars().count(), 200);
        assert!(feedback.text.ends_with('…'), "{feedback:?}");
        assert!(feedback.show_alert, "{feedback:?}");
    }

    #[test]
    fn telegram_choice_keyboard_skips_callbacks_too_large_for_telegram() {
        let long_id = "x".repeat(80);
        let skipped: Vec<_> = (0..20)
            .map(|idx| {
                crate::messages::ChoiceOption::new(
                    format!("Too long {idx}"),
                    format!("!agent switch {long_id}-{idx}"),
                )
                .with_callback_data(format!("cf:agent:{long_id}-{idx}"))
            })
            .collect();
        let message =
            OutboundMessage::text("Choose").with_control(crate::messages::ChoiceControl::new(
                "Options",
                skipped
                    .into_iter()
                    .chain(std::iter::once(
                        crate::messages::ChoiceOption::new("Short", "!agent switch short")
                            .with_callback_data("cf:agent:short"),
                    ))
                    .collect(),
            ));

        let keyboard = choice_keyboard_from_controls(&message).expect("one valid button");
        let buttons: Vec<_> = keyboard.inline_keyboard.into_iter().flatten().collect();

        assert_eq!(buttons.len(), 1);
        assert_eq!(buttons[0].text, "Short");
    }

    #[test]
    fn telegram_choice_keyboard_truncates_long_button_text() {
        let long_label = format!("session-{}", "x".repeat(80));
        let message =
            OutboundMessage::text("Choose").with_control(crate::messages::ChoiceControl::new(
                "Options",
                vec![
                    crate::messages::ChoiceOption::new(long_label, "!switch claude-acpx backend")
                        .with_callback_data("cf:session:claude-acpx:backend"),
                ],
            ));

        let keyboard = choice_keyboard_from_controls(&message).expect("one valid button");
        let buttons: Vec<_> = keyboard.inline_keyboard.into_iter().flatten().collect();

        assert_eq!(buttons.len(), 1);
        assert_eq!(buttons[0].text.chars().count(), 64);
        assert!(buttons[0].text.ends_with("..."));
    }

    #[test]
    fn telegram_choice_keyboard_renders_session_and_approval_controls() {
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

        let keyboard = choice_keyboard_from_controls(&message).expect("choice buttons");
        let buttons: Vec<_> = keyboard.inline_keyboard.into_iter().flatten().collect();

        assert_eq!(buttons.len(), 3);
        assert_eq!(buttons[0].text, "backend");
        assert_eq!(buttons[1].text, "Approve");
        assert_eq!(buttons[2].text, "Deny");
        assert_eq!(
            parse_callback_action("cf:session:claude-acpx:backend"),
            Some(TelegramCallbackAction::Session {
                agent_id: "claude-acpx",
                session: "backend"
            })
        );
        assert_eq!(
            parse_callback_action("cf:approve:req-1"),
            Some(TelegramCallbackAction::Approve("req-1"))
        );
        assert_eq!(
            parse_callback_action("cf:deny:req-1"),
            Some(TelegramCallbackAction::Deny("req-1"))
        );
    }

    #[test]
    fn telegram_choice_falls_back_to_text_when_rich_ui_disabled() {
        let message =
            OutboundMessage::text("Choose").with_control(crate::messages::ChoiceControl::new(
                "Options",
                vec![
                    crate::messages::ChoiceOption::new("Librarian", "!agent switch librarian")
                        .with_callback_data("cf:agent:librarian"),
                ],
            ));

        assert!(
            telegram_keyboard_for_message(&message, false).is_none(),
            "ui_mode=text must suppress native Telegram buttons"
        );
        let rendered = message.render_text_fallback();
        assert!(
            rendered.contains("!agent switch librarian"),
            "text fallback must preserve the actionable command: {rendered}"
        );
    }

    #[test]
    fn test_context_key_isolates_identities_in_same_chat() {
        // Reproduces Bug 2: when Brian switches from one identity to another in the
        // same Telegram chat, the new identity must NOT receive the previous identity's context.
        let store = ContextStore::new(20, 5);

        // Simulate a Max/David conversation: chat_id=42, identity="max"
        let max_key = "42-max";
        store.push(max_key, "David", "max question", "librarian", "max answer");

        // Now Brian switches to ironclaw (identity="brian") in the same chat
        let brian_key = "42-brian";

        // brian's context key is different — should see NO preamble for its first message
        let out = store.augment_message(brian_key, "ironclaw", "fresh ironclaw question");
        assert_eq!(
            out, "fresh ironclaw question",
            "ironclaw should not see max's context in the same chat: {}",
            out
        );

        // Conversely, max's context key should still have history
        let max_out = store.augment_message(max_key, "custodian", "another max question");
        assert!(
            max_out.contains("max question"),
            "max's context should still have history: {}",
            max_out
        );
    }

    #[test]
    fn test_context_key_format() {
        // Verify the key format used: "{chat_id}-{identity_id}"
        let chat_id: i64 = 7000000001;
        let identity_id = "brian";
        let key = format!("{}-{}", chat_id, identity_id);
        assert_eq!(key, "7000000001-brian");
    }

    // -----------------------------------------------------------------------
    // Non-blocking command path: verify commands return Some(reply) synchronously
    // without any async await.  This is the core of the fix — commands must not
    // block on agent I/O.
    // -----------------------------------------------------------------------

    #[test]
    fn test_commands_return_reply_synchronously_no_await() {
        // CommandHandler::handle() is a plain synchronous fn — no futures, no await.
        // If it returns Some(_), the reply is ready immediately and can be sent
        // in a spawned task without blocking the Teloxide dispatcher.
        let config = Arc::new(make_test_config());
        let handler = make_handler(config);

        // All of these should return Some immediately, with no I/O or blocking.
        // NOTE: !status is intentionally excluded — it requires identity context
        // (post-auth) and is handled via cmd_status_for_identity(), not handle().
        let cases = [
            "!ping", "!help", "!agents", "!metrics", // Case variants
            "!PING", "!Help",
        ];

        for cmd in &cases {
            let result = handler.handle(cmd);
            assert!(
                result.is_some(),
                "command '{}' must return Some(reply) synchronously",
                cmd
            );
            // Confirm it doesn't return an empty string — that would be a silent failure
            assert!(
                !result.unwrap().is_empty(),
                "command '{}' must return a non-empty reply",
                cmd
            );
        }

        // !status now requires identity context — must return None from handle()
        // so the dispatcher can resolve identity first, then call cmd_status_for_identity().
        assert!(
            handler.handle("!status").is_none(),
            "!status must NOT be handled pre-auth (returns None from handle())"
        );
        assert!(
            handler.handle("!STATUS").is_none(),
            "!STATUS must NOT be handled pre-auth"
        );

        // !switch without args — handled by handle_switch (post-auth), NOT handle()
        // handle() must return None for !switch so the caller can do auth first.
        assert!(
            handler.handle("!switch librarian").is_none(),
            "!switch must NOT be handled pre-auth (returns None from handle())"
        );

        // !context clear — also not in handle(), handled inline in the dispatcher.
        // Verify it's not accidentally consumed by handle().
        assert!(
            handler.handle("!context clear").is_none(),
            "!context clear must NOT be handled by CommandHandler::handle()"
        );

        // Non-commands must still return None (fall-through to agent).
        assert!(handler.handle("hello agent").is_none());
        assert!(handler.handle("what is the weather?").is_none());
    }

    #[tokio::test]
    async fn test_status_command_is_post_auth() {
        // cmd_status_for_identity() is now async — it queries the adapter for runtime status.
        // Verify it returns a non-empty, identity-aware String.
        let config = Arc::new(make_test_config());
        let handler = make_handler(config);

        let reply = handler.cmd_status_for_identity("brian").await;
        assert!(
            !reply.is_empty(),
            "cmd_status_for_identity must return non-empty String"
        );
        assert!(
            reply.contains("librarian"),
            "status should show active agent for brian: {}",
            reply
        );
        assert!(
            reply.contains("version:"),
            "status should include version: {}",
            reply
        );
        assert!(
            reply.contains("uptime:"),
            "status should include uptime: {}",
            reply
        );
    }

    #[test]
    fn test_switch_command_reply_is_synchronous() {
        // handle_switch() is also a plain synchronous fn — critical for the fix.
        // Verify it returns a non-empty String without any async machinery.
        let config = Arc::new(make_test_config());
        let handler = make_handler(config);

        let reply = handler.handle_switch("!switch librarian", "brian");
        assert!(
            !reply.is_empty(),
            "handle_switch must return non-empty String synchronously"
        );
        // A successful switch should include ✅
        assert!(
            reply.contains('✅'),
            "successful switch should confirm: {}",
            reply
        );
    }
}
