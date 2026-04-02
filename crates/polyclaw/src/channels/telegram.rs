//! Telegram channel adapter using teloxide.
//!
//! Listens for messages via long polling, enforces the allow_list at the
//! boundary, routes to the downstream agent, and sends the reply back.

use anyhow::{Context, Result};
use std::sync::Arc;
use teloxide::{
    prelude::*,
    types::{Me, ParseMode},
};
use tracing::{debug, info, warn};

use crate::{
    auth::{find_agent, resolve_telegram_sender},
    commands::CommandHandler,
    config::{expand_tilde, PolyConfig},
    context::ContextStore,
    router::Router,
};

/// Run the Telegram bot until shutdown.
pub async fn run(
    config: Arc<PolyConfig>,
    router: Arc<Router>,
    command_handler: Arc<CommandHandler>,
    context_store: ContextStore,
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

    let handler =
        Update::filter_message().branch(dptree::entry().endpoint(move |bot: Bot, msg: Message| {
            let cfg = config_clone.clone();
            let rtr = router_clone.clone();
            let cmd = cmd_handler_clone.clone();
            let ctx = ctx_store_clone.clone();
            async move {
                // Dispatch synchronously for commands, spawn for agent calls.
                // This ensures !commands respond immediately even if a prior
                // agent dispatch is still running (Teloxide serialises per chat_id).
                handle_message_nonblocking(bot, msg, cfg, rtr, cmd, ctx);
                respond(())
            }
        }));

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
    config: Arc<PolyConfig>,
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
    let user = match msg.from() {
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

    info!(
        identity = %identity.id,
        sender_id = %sender_id,
        text_len = %text.len(),
        "authorized message from identity"
    );

    // Context key: scoped to (chat_id, identity_id) so each identity has isolated
    // conversation history even within the same Telegram chat.
    let chat_key = format!("{}-{}", chat_id.0, identity.id);

    // -----------------------------------------------------------------------
    // Command fast-path — all handled synchronously, reply spawned immediately.
    // These return before the handler, keeping the Teloxide dispatcher free.
    // -----------------------------------------------------------------------

    // Pre-auth-safe commands — no identity context needed.
    if let Some(reply) = command_handler.handle(&text) {
        debug!(chat_id = %chat_id, cmd = %text.trim(), "handled local pre-auth command");
        let bot2 = bot.clone();
        tokio::spawn(async move {
            if let Err(e) = bot2.send_message(chat_id, &reply).await {
                warn!(chat_id = %chat_id, error = %e, "failed to send command reply");
            }
        });
        return;
    }

    // If the text looks like a !command but wasn't handled as a pre-auth
    // local command and it is NOT a post-auth command (status/switch/default/sessions),
    // reply with a helpful unknown-command message rather than routing it to an agent.
    if CommandHandler::is_command(&text)
        && !CommandHandler::is_status_command(&text)
        && !CommandHandler::is_switch_command(&text)
        && !CommandHandler::is_default_command(&text)
        && !CommandHandler::is_sessions_command(&text)
    {
        let reply = command_handler.unknown_command(&text);
        let bot2 = bot.clone();
        tokio::spawn(async move {
            if let Err(e) = bot2.send_message(chat_id, &reply).await {
                warn!(chat_id = %chat_id, error = %e, "failed to send unknown-command reply");
            }
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
            let reply = command_handler2.cmd_status_for_identity(&identity_id).await;
            if let Err(e) = bot2.send_message(chat_id, &reply).await {
                warn!(chat_id = %chat_id, error = %e, "failed to send status reply");
            }
        });
        return;
    }

    // !switch — requires identity context; handled post-auth.
    if CommandHandler::is_switch_command(&text) {
        debug!(chat_id = %chat_id, identity = %identity.id, "handling !switch command");
        let reply = command_handler.handle_switch(&text, &identity.id);
        let bot2 = bot.clone();
        tokio::spawn(async move {
            if let Err(e) = bot2.send_message(chat_id, &reply).await {
                warn!(chat_id = %chat_id, error = %e, "failed to send switch reply");
            }
        });
        return;
    }

    // !sessions — list ACP sessions for an agent; requires identity context.
    if CommandHandler::is_sessions_command(&text) {
        debug!(chat_id = %chat_id, identity = %identity.id, "handling !sessions command");
        let bot2 = bot.clone();
        let identity_id = identity.id.clone();
        let command_handler2 = command_handler.clone();
        tokio::spawn(async move {
            let reply = command_handler2.handle_sessions(&text, &identity_id).await;
            if let Err(e) = bot2.send_message(chat_id, &reply).await {
                warn!(chat_id = %chat_id, error = %e, "failed to send sessions reply");
            }
        });
        return;
    }

    // !default — switch back to configured default agent; requires identity context.
    if CommandHandler::is_default_command(&text) {
        debug!(chat_id = %chat_id, identity = %identity.id, "handling !default command");
        let reply = command_handler.handle_default(&identity.id);
        let bot2 = bot.clone();
        tokio::spawn(async move {
            if let Err(e) = bot2.send_message(chat_id, &reply).await {
                warn!(chat_id = %chat_id, error = %e, "failed to send default reply");
            }
        });
        return;
    }

    // !context clear — clear the conversation buffer for this chat.
    if text.trim().eq_ignore_ascii_case("!context clear") {
        context_store.clear(&chat_key);
        let bot2 = bot.clone();
        tokio::spawn(async move {
            if let Err(e) = bot2
                .send_message(chat_id, "🧹 Conversation context cleared.")
                .await
            {
                warn!(chat_id = %chat_id, error = %e, "failed to send context-clear reply");
            }
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
            if let Some((ack, follow_up)) = cmd.handle_async(&text_owned).await {
                let _ = bot2.send_message(chat_id, &ack).await;
                if let Some(resp) = follow_up {
                    // Try MarkdownV2 for the continuation agent response; fall back to plain.
                    let send_result = bot2
                        .send_message(chat_id, &resp)
                        .parse_mode(ParseMode::MarkdownV2)
                        .await;
                    if send_result.is_err() {
                        let _ = bot2.send_message(chat_id, &resp).await;
                    }
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
                let _ = bot2.send_message(chat_id, "⚠️ Agent not configured.").await;
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

    // Spawn the agent dispatch — handler returns immediately.
    // All slow I/O (context augment, HTTP, reply send) happens in the task.
    tokio::spawn(async move {
        // Augment message with conversation context (unseen exchanges for this agent).
        let augmented_text = context_store.augment_message(&chat_key, &agent_id, &text);

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
            .dispatch_with_sender(&augmented_text, &agent, &config, Some(&identity.id))
            .await
        {
            Ok(response) => {
                let latency_ms = dispatch_start.elapsed().as_millis() as u64;
                command_handler.record_dispatch(latency_ms);
                debug!(
                    identity = %identity.id,
                    agent_id = %agent_id,
                    response_len = %response.len(),
                    "got agent response"
                );

                // Record the exchange (original, un-augmented prompt) in the context buffer.
                context_store.push(&chat_key, &sender_label, &text, &agent_id, &response);

                // Send response — try MarkdownV2 first, fall back to plain text.
                let send_result = bot
                    .send_message(chat_id, &response)
                    .parse_mode(ParseMode::MarkdownV2)
                    .await;
                if send_result.is_err() {
                    let _ = bot.send_message(chat_id, &response).await;
                }
            }
            Err(e) => {
                // ── Clash approval flow ─────────────────────────────────────
                // Check if the agent loop paused for human approval.
                if let Some(approval) = e.downcast_ref::<crate::adapters::AdapterError>() {
                    if let crate::adapters::AdapterError::ApprovalPending(req) = approval {
                        let req = req.clone();
                        debug!(
                            request_id = %req.request_id,
                            command = %req.command,
                            "clash: forwarding approval request to user"
                        );
                        // Register in command handler so !approve / !deny can find it.
                        command_handler.register_pending_approval(
                            crate::adapters::openclaw::PendingApprovalMeta {
                                request_id: req.request_id.clone(),
                                nzc_endpoint: agent.endpoint.clone(),
                                nzc_auth_token: agent
                                    .auth_token
                                    .clone()
                                    .unwrap_or_default(),
                                summary: format!(
                                    "🔒 Approval required\nCommand: {}\nReason: {}\nReply !approve or !deny [reason]\nRequest ID: {}",
                                    req.command, req.reason, req.request_id
                                ),
                            },
                        ).await;

                        // Send the approval notification to the user.
                        let notification = format!(
                            "🔒 Approval required\nCommand: {}\nReason: {}\nReply !approve or !deny [reason]\nRequest ID: {}",
                            req.command, req.reason, req.request_id
                        );
                        let _ = bot.send_message(chat_id, &notification).await;
                        return; // Don't send an error — we already notified.
                    }
                }
                // ─────────────────────────────────────────────────────────────
                warn!(identity = %identity.id, error = %e, "agent dispatch failed");
                let _ = bot
                    .send_message(chat_id, &format!("⚠️ Agent error: {}", e))
                    .await;
            }
        }
    });
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
    config: Arc<PolyConfig>,
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
    let user = match msg.from() {
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
    // This prevents context bleed when Brian switches between identities (e.g. Max → IronClaw).
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

    // !sessions — list ACP sessions for an agent; requires identity context.
    if CommandHandler::is_sessions_command(&text) {
        debug!(chat_id = %chat_id, identity = %identity.id, "handling !sessions command");
        let reply = command_handler.handle_sessions(&text, &identity.id).await;
        if let Err(e) = bot.send_message(chat_id, &reply).await {
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

    // Augment message with conversation context (unseen exchanges for this agent).
    let augmented_text = context_store.augment_message(&chat_key, &agent_id, &text);

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
        .dispatch_with_sender(&augmented_text, &agent, &config, Some(&identity.id))
        .await
    {
        Ok(response) => {
            let latency_ms = dispatch_start.elapsed().as_millis() as u64;
            command_handler.record_dispatch(latency_ms);
            debug!(
                identity = %identity.id,
                agent_id = %agent_id,
                response_len = %response.len(),
                "got agent response"
            );

            // Record the exchange (original, un-augmented prompt) in the context buffer.
            context_store.push(&chat_key, &sender_label, &text, &agent_id, &response);

            // Send response — try MarkdownV2 first, fall back to plain text.
            let send_result = bot
                .send_message(chat_id, &response)
                .parse_mode(ParseMode::MarkdownV2)
                .await;
            if send_result.is_err() {
                let _ = bot.send_message(chat_id, &response).await;
            }
        }
        Err(e) => {
            warn!(identity = %identity.id, error = %e, "agent dispatch failed");
            let _ = bot
                .send_message(chat_id, &format!("⚠️ Agent error: {}", e))
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
        AgentConfig, ChannelAlias, ChannelConfig, Identity, PolyConfig, PolyHeader, RoutingRule,
    };

    /// Create a CommandHandler backed by a temp state directory so tests are
    /// isolated from `~/.polyclaw/state/active-agents.json` on disk.
    fn make_handler(config: Arc<PolyConfig>) -> CommandHandler {
        let tmp = tempfile::tempdir().expect("tempdir for telegram test state isolation");
        CommandHandler::with_state_dir(config, tmp.path().to_path_buf())
    }

    fn make_test_config() -> PolyConfig {
        PolyConfig {
            polyclaw: PolyHeader { version: 2 },
            identities: vec![Identity {
                id: "brian".to_string(),
                display_name: Some("Brian".to_string()),
                aliases: vec![ChannelAlias {
                    channel: "telegram".to_string(),
                    id: "8465871195".to_string(),
                }],
                role: Some("owner".to_string()),
            }],
            agents: vec![AgentConfig {
                id: "librarian".to_string(),
                kind: "openclaw-http".to_string(),
                endpoint: "http://10.0.0.20:18789".to_string(),
                timeout_ms: Some(120000),
                model: None,
                auth_token: Some("REPLACE_WITH_AUTH_TOKEN".to_string()),
                api_key: None,
                openclaw_agent_id: None,
                reply_port: None,
                reply_auth_token: None,
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
                bot_token_file: Some("~/.polyclaw/secrets/telegram-token".to_string()),
                enabled: true,
                ..Default::default()
            }],
            permissions: None,
            memory: None,
            context: Default::default(),
        }
    }

    #[test]
    fn test_auth_resolves_known_sender() {
        let config = make_test_config();
        let id = resolve_telegram_sender(8465871195, &config);
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
        let chat_id: i64 = 8465871195;
        let identity_id = "brian";
        let key = format!("{}-{}", chat_id, identity_id);
        assert_eq!(key, "8465871195-brian");
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
