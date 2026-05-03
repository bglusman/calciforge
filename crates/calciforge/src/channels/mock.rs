//! Mock channel for testing and development.
//!
//! This channel simulates user interactions without external dependencies.
//! It can be controlled via HTTP API to inject test messages and verify responses.
//!
//! Configuration:
//! ```toml
//! [[channels]]
//! kind = "mock"
//! enabled = true
//! ```
//!
//! The control port defaults to 9090 and can be overridden via `control_port` in channel config.
//!
//! The `/send` endpoint routes through the same command handler and adapter
//! router used by production channels, while keeping all ingress and egress on
//! localhost. This makes it useful for local smoke tests that should not depend
//! on Telegram, Matrix, or another external channel.

use anyhow::{Context, Result};
use axum::{
    extract::State,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::info;

use crate::sync::Arc;

use crate::{
    auth::{find_agent, resolve_channel_sender, ResolvedIdentity},
    choice_state::{ChoiceMatchResult, ChoiceState},
    commands::CommandHandler,
    config::CalciforgeConfig,
    context::ContextStore,
    router::{Router as CalciforgeRouter, RouterDispatchContext},
};

/// Mock channel state
#[derive(Clone)]
#[allow(dead_code)]
struct MockState {
    /// Messages sent by the mock channel (for verification)
    sent_messages: Arc<Mutex<Vec<MockMessage>>>,
    /// Messages received by the mock channel (from test API)
    received_messages: Arc<Mutex<Vec<MockMessage>>>,
    /// Router for sending to agents
    router: Arc<CalciforgeRouter>,
    /// Command handler
    command_handler: Arc<CommandHandler>,
    /// Context store
    context_store: ContextStore,
    /// Configuration
    config: Arc<CalciforgeConfig>,
    /// Per-identity pending-choice tracker
    choice_state: Arc<ChoiceState>,
    #[cfg(test)]
    _state_dir: Option<Arc<tempfile::TempDir>>,
}

/// A mock message for logging
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MockMessage {
    /// Message ID
    id: String,
    /// Sender ID
    sender: String,
    /// Message text
    text: String,
    /// Timestamp (ISO 8601)
    timestamp: String,
    /// Response (if any)
    response: Option<String>,
}

/// API request to send a test message
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct SendMessageRequest {
    /// Sender ID (must be in allowed_users/test_users)
    sender: String,
    /// Message text
    text: String,
    /// Optional: Expected response pattern (for verification)
    expected_response: Option<String>,
}

/// API response
#[derive(Debug, Serialize)]
struct ApiResponse {
    success: bool,
    message: String,
    data: Option<serde_json::Value>,
}

/// Run the mock channel
pub async fn run(
    config: Arc<CalciforgeConfig>,
    router: Arc<CalciforgeRouter>,
    command_handler: Arc<CommandHandler>,
    context_store: ContextStore,
    choice_state: Arc<ChoiceState>,
) -> Result<()> {
    // Find the mock channel config
    let mock_channel = config
        .channels
        .iter()
        .find(|c| c.kind == "mock" && c.enabled)
        .context("no enabled mock channel found in config")?;

    info!("Starting mock channel");

    // Create state
    let state = MockState {
        sent_messages: Arc::new(Mutex::new(Vec::new())),
        received_messages: Arc::new(Mutex::new(Vec::new())),
        router: router.clone(),
        command_handler: command_handler.clone(),
        context_store: context_store.clone(),
        config: config.clone(),
        choice_state,
        #[cfg(test)]
        _state_dir: None,
    };

    let control_port = mock_channel.control_port.unwrap_or(9090);

    // Start control API server
    let control_app = Router::new()
        .route("/health", get(health_handler))
        .route("/messages", get(get_messages_handler))
        .route("/send", post(send_message_handler))
        .route("/clear", post(clear_messages_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", control_port))
        .await
        .context(format!("failed to bind to port {}", control_port))?;

    info!(
        "Mock channel control API listening on port {}",
        control_port
    );
    info!("Use HTTP API to send test messages:");
    info!("  GET  http://127.0.0.1:{}/health", control_port);
    info!("  GET  http://127.0.0.1:{}/messages", control_port);
    info!("  POST http://127.0.0.1:{}/send", control_port);
    info!("  POST http://127.0.0.1:{}/clear", control_port);

    axum::serve(listener, control_app)
        .await
        .context("mock channel control API server failed")?;

    Ok(())
}

/// Health check handler
async fn health_handler() -> impl IntoResponse {
    Json(ApiResponse {
        success: true,
        message: "Mock channel control API is healthy".to_string(),
        data: None,
    })
}

/// Get all messages handler
async fn get_messages_handler(State(state): State<MockState>) -> impl IntoResponse {
    let sent = state.sent_messages.lock().await;
    let received = state.received_messages.lock().await;

    Json(ApiResponse {
        success: true,
        message: format!(
            "{} sent messages, {} received messages",
            sent.len(),
            received.len()
        ),
        data: Some(serde_json::json!({
            "sent": &*sent,
            "received": &*received,
        })),
    })
}

/// Send a test message handler
async fn send_message_handler(
    State(state): State<MockState>,
    Json(req): Json<SendMessageRequest>,
) -> impl IntoResponse {
    let message_id = format!("mock-{}", chrono::Utc::now().timestamp_millis());
    let timestamp = chrono::Utc::now().to_rfc3339();

    // Create mock message
    let mock_message = MockMessage {
        id: message_id.clone(),
        sender: req.sender.clone(),
        text: req.text.clone(),
        timestamp: timestamp.clone(),
        response: None,
    };

    // Store in received messages
    {
        let mut received = state.received_messages.lock().await;
        received.push(mock_message.clone());
    }

    info!(sender = %req.sender, text = %req.text, "Mock channel received message");

    let response_result = route_mock_message(&state, &req.sender, &req.text).await;
    let (success, response_text) = match response_result {
        Ok(response) => (true, response),
        Err(err) => (false, err),
    };

    // Create response message
    let response_message = MockMessage {
        id: format!("resp-{}", message_id),
        sender: "system".to_string(),
        text: response_text.clone(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        response: None,
    };

    // Store in sent messages
    {
        let mut sent = state.sent_messages.lock().await;
        sent.push(response_message);
    }

    // Update original message with response
    {
        let mut received = state.received_messages.lock().await;
        if let Some(msg) = received.iter_mut().find(|m| m.id == message_id) {
            msg.response = Some(response_text.clone());
        }
    }

    Json(ApiResponse {
        success,
        message: if success {
            "Message routed".to_string()
        } else {
            "Message routing failed".to_string()
        },
        data: Some(serde_json::json!({
            "message_id": message_id,
            "response": response_text,
        })),
    })
}

async fn route_mock_message(
    state: &MockState,
    sender: &str,
    text: &str,
) -> std::result::Result<String, String> {
    let identity = resolve_mock_sender(sender, &state.config).ok_or_else(|| {
        format!("Unknown mock sender '{sender}'. Add a mock alias or use a configured identity id.")
    })?;
    let identity_id = identity.id;
    let chat_key = format!("mock-{}-{}", sender, identity_id);

    // ── Pending-choice resolution (free-text only) ───────────────────
    match state.choice_state.match_reply("mock", &identity_id, text) {
        ChoiceMatchResult::Match { command, .. } => {
            if CommandHandler::is_switch_command(&command) {
                return Ok(state.command_handler.handle_switch(&command, &identity_id));
            }
            if CommandHandler::is_model_command(&command) {
                return Ok(state.command_handler.handle_model(&command, &identity_id));
            }
            if CommandHandler::is_approve_command(&command)
                || CommandHandler::is_deny_command(&command)
            {
                if let Some((ack, follow_up)) = state.command_handler.handle_async(&command).await {
                    return Ok(match follow_up {
                        Some(resp) => format!("{ack}\n\n{resp}"),
                        None => ack,
                    });
                }
            }
            if let Some(reply) = state.command_handler.handle(&command) {
                return Ok(reply);
            }
            return Err(format!("Resolved choice command not handled: {command}"));
        }
        ChoiceMatchResult::Ambiguous => {
            return Ok(
                "Multiple options match. Reply with the number, or be more specific.".to_string(),
            );
        }
        ChoiceMatchResult::OutOfRange => {
            return Ok("That number isn't one of the options. Reply with a number from the list, or the option name.".to_string());
        }
        _ => {}
    }

    if let Some(reply) = state
        .command_handler
        .agent_choice_message_for_identity(text, &identity_id)
    {
        if !reply.controls.is_empty() {
            state
                .choice_state
                .record("mock", &identity_id, reply.controls.clone());
        }
        return Ok(reply.render_text_fallback());
    }

    if let Some(reply) = state.command_handler.model_choice_message(text) {
        if !reply.controls.is_empty() {
            state
                .choice_state
                .record("mock", &identity_id, reply.controls.clone());
        }
        return Ok(reply.render_text_fallback());
    }

    if let Some(reply) = state.command_handler.handle(text) {
        return Ok(reply);
    }

    if CommandHandler::is_command(text)
        && !CommandHandler::is_status_command(text)
        && !CommandHandler::is_switch_command(text)
        && !CommandHandler::is_default_command(text)
        && !CommandHandler::is_sessions_command(text)
        && !CommandHandler::is_model_command(text)
        && !CommandHandler::is_secure_command(text)
        && !CommandHandler::is_approve_command(text)
        && !CommandHandler::is_deny_command(text)
    {
        return Ok(state.command_handler.unknown_command(text));
    }

    if CommandHandler::is_status_command(text) {
        return Ok(state
            .command_handler
            .cmd_status_for_identity(&identity_id)
            .await);
    }

    if CommandHandler::is_switch_command(text) {
        return Ok(state.command_handler.handle_switch(text, &identity_id));
    }

    if CommandHandler::is_model_command(text) {
        return Ok(state.command_handler.handle_model(text, &identity_id));
    }

    if CommandHandler::is_sessions_command(text) {
        let reply = state
            .command_handler
            .handle_sessions_message(text, &identity_id)
            .await;
        if !reply.controls.is_empty() {
            state
                .choice_state
                .record("mock", &identity_id, reply.controls.clone());
        }
        return Ok(reply.render_text_fallback());
    }

    if CommandHandler::is_default_command(text) {
        return Ok(state.command_handler.handle_default(&identity_id));
    }

    if CommandHandler::is_secure_command(text) {
        if CommandHandler::is_secure_set_command(text)
            && !crate::config::channel_allows_chat_secret_set(&state.config, "mock")
        {
            return Ok(CommandHandler::secure_set_disabled_reply("Mock"));
        }
        return Ok(state
            .command_handler
            .handle_secure(text, &identity_id)
            .await);
    }

    if text.trim().eq_ignore_ascii_case("!context clear") {
        state.context_store.clear(&chat_key);
        return Ok("Conversation context cleared.".to_string());
    }

    if CommandHandler::is_approve_command(text) || CommandHandler::is_deny_command(text) {
        if let Some((ack, follow_up)) = state.command_handler.handle_async(text).await {
            return Ok(match follow_up {
                Some(follow_up) => format!("{ack}\n\n{follow_up}"),
                None => ack,
            });
        }
        return Ok(state.command_handler.unknown_command(text));
    }

    let agent_id = state
        .command_handler
        .active_agent_for(&identity_id)
        .ok_or_else(|| format!("No routing rule configured for identity '{identity_id}'."))?;
    let agent = find_agent(&agent_id, &state.config)
        .cloned()
        .ok_or_else(|| format!("Agent '{agent_id}' is not configured."))?;
    let sender_label = state
        .config
        .identities
        .iter()
        .find(|identity| identity.id == identity_id)
        .and_then(|identity| identity.display_name.as_deref())
        .unwrap_or(&identity_id)
        .to_string();
    let preserve_native_commands = crate::adapters::agent_supports_native_commands(&agent);
    let augmented = state.context_store.augment_message_with_options(
        &chat_key,
        &agent_id,
        text,
        preserve_native_commands,
    );
    let model_override = state
        .command_handler
        .active_model_for_identity(&identity_id);
    let selected_session = state
        .command_handler
        .active_session_for(&identity_id, &agent_id);

    let dispatch_start = std::time::Instant::now();
    match state
        .router
        .dispatch_message_with_full_context(
            &augmented,
            &agent,
            &state.config,
            RouterDispatchContext {
                sender: Some(&identity_id),
                model_override: model_override.as_deref(),
                session: selected_session.as_deref(),
                channel: Some("mock"),
            },
        )
        .await
    {
        Ok(response_message) => {
            if !response_message.controls.is_empty() {
                state
                    .choice_state
                    .record("mock", &identity_id, response_message.controls.clone());
            }
            let response = response_message.render_text_fallback();
            state
                .command_handler
                .record_dispatch(dispatch_start.elapsed().as_millis() as u64);
            state.context_store.push_with_options(
                &chat_key,
                &sender_label,
                text,
                &agent_id,
                &response,
                preserve_native_commands,
            );
            Ok(response)
        }
        Err(err) => {
            if let Some(crate::adapters::AdapterError::ApprovalPending(req)) =
                err.downcast_ref::<crate::adapters::AdapterError>()
            {
                state
                    .command_handler
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
                return Ok(CommandHandler::approval_request_message(
                    &req.command,
                    &req.reason,
                    &req.request_id,
                )
                .render_text_fallback());
            }
            Err(format!("Agent error: {err}"))
        }
    }
}

fn resolve_mock_sender(sender: &str, config: &CalciforgeConfig) -> Option<ResolvedIdentity> {
    resolve_channel_sender("mock", sender, config).or_else(|| {
        config
            .identities
            .iter()
            .find(|identity| identity.id == sender)
            .map(|identity| ResolvedIdentity {
                id: identity.id.clone(),
                role: identity.role.clone(),
            })
    })
}

/// Clear all messages handler
async fn clear_messages_handler(State(state): State<MockState>) -> impl IntoResponse {
    {
        let mut sent = state.sent_messages.lock().await;
        sent.clear();

        let mut received = state.received_messages.lock().await;
        received.clear();
    }

    Json(ApiResponse {
        success: true,
        message: "All messages cleared".to_string(),
        data: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AgentConfig, CalciforgeConfig, CalciforgeHeader, ChannelAlias, ChannelConfig, Identity,
        RoutingRule,
    };
    use std::collections::HashMap;

    fn agent(id: &str) -> AgentConfig {
        AgentConfig {
            id: id.to_string(),
            kind: "cli".to_string(),
            endpoint: String::new(),
            timeout_ms: Some(5_000),
            model: None,
            auth_token: None,
            api_key: None,
            api_key_file: None,
            openclaw_agent_id: None,
            allow_model_override: None,
            reply_port: None,
            reply_auth_token: None,
            reply_auth_token_file: None,
            command: Some("/bin/echo".to_string()),
            args: Some(vec!["{message}".to_string()]),
            env: Some(HashMap::new()),
            registry: None,
            aliases: vec![],
        }
    }

    fn config_with_mock_identity() -> Arc<CalciforgeConfig> {
        Arc::new(CalciforgeConfig {
            calciforge: CalciforgeHeader { version: 2 },
            identities: vec![Identity {
                id: "brian".to_string(),
                display_name: Some("Brian".to_string()),
                aliases: vec![ChannelAlias {
                    channel: "mock".to_string(),
                    id: "mock-brian".to_string(),
                }],
                role: Some("owner".to_string()),
            }],
            agents: vec![agent("echo")],
            routing: vec![RoutingRule {
                identity: "brian".to_string(),
                default_agent: "echo".to_string(),
                allowed_agents: vec!["echo".to_string()],
            }],
            alloys: vec![],
            cascades: vec![],
            dispatchers: vec![],
            exec_models: vec![],
            channels: vec![ChannelConfig {
                kind: "mock".to_string(),
                enabled: true,
                control_port: Some(0),
                ..Default::default()
            }],
            permissions: None,
            memory: None,
            context: Default::default(),
            model_shortcuts: vec![],
            security: None,
            proxy: None,
            local_models: None,
        })
    }

    fn state(config: Arc<CalciforgeConfig>) -> MockState {
        let temp = tempfile::tempdir().expect("mock command handler state dir");
        MockState {
            sent_messages: Arc::new(Mutex::new(Vec::new())),
            received_messages: Arc::new(Mutex::new(Vec::new())),
            router: Arc::new(CalciforgeRouter::new()),
            command_handler: Arc::new(CommandHandler::with_state_dir(
                config.clone(),
                temp.path().to_path_buf(),
            )),
            context_store: ContextStore::new(4, 4),
            config,
            choice_state: Arc::new(ChoiceState::new()),
            _state_dir: Some(Arc::new(temp)),
        }
    }

    #[tokio::test]
    async fn mock_api_routes_messages_to_the_active_agent() {
        let state = state(config_with_mock_identity());

        let response = route_mock_message(&state, "mock-brian", "hello mock")
            .await
            .expect("mock API should route to the configured agent");

        assert_eq!(response, "hello mock");
    }

    #[tokio::test]
    async fn mock_api_rejects_unknown_senders_before_dispatch() {
        let state = state(config_with_mock_identity());

        let response = route_mock_message(&state, "unknown", "hello mock").await;

        assert!(
            response
                .expect_err("unknown sender should fail")
                .contains("Unknown mock sender"),
            "unknown senders must not reach agent dispatch"
        );
    }

    fn config_with_two_agents() -> Arc<CalciforgeConfig> {
        Arc::new(CalciforgeConfig {
            calciforge: CalciforgeHeader { version: 2 },
            identities: vec![Identity {
                id: "brian".to_string(),
                display_name: Some("Brian".to_string()),
                aliases: vec![ChannelAlias {
                    channel: "mock".to_string(),
                    id: "mock-brian".to_string(),
                }],
                role: Some("owner".to_string()),
            }],
            agents: vec![agent("librarian"), agent("critic")],
            routing: vec![RoutingRule {
                identity: "brian".to_string(),
                default_agent: "librarian".to_string(),
                allowed_agents: vec!["librarian".to_string(), "critic".to_string()],
            }],
            alloys: vec![],
            cascades: vec![],
            dispatchers: vec![],
            exec_models: vec![],
            channels: vec![ChannelConfig {
                kind: "mock".to_string(),
                enabled: true,
                control_port: Some(0),
                ..Default::default()
            }],
            permissions: None,
            memory: None,
            context: Default::default(),
            model_shortcuts: vec![],
            security: None,
            proxy: None,
            local_models: None,
        })
    }

    #[tokio::test]
    async fn choice_state_free_text_match_by_number() {
        use crate::messages::{ChoiceControl, ChoiceOption};

        let s = state(config_with_two_agents());
        let ctrl = ChoiceControl::new(
            "Pick agent",
            vec![
                ChoiceOption::agent("Librarian", "librarian"),
                ChoiceOption::agent("Critic", "critic"),
            ],
        );
        s.choice_state.record("mock", "brian", vec![ctrl]);

        let reply = route_mock_message(&s, "brian", "1")
            .await
            .expect("numeric reply should resolve");
        assert!(
            reply.contains("librarian"),
            "reply should confirm switch to librarian: {reply}"
        );
        assert_eq!(
            s.choice_state.pending_len(),
            0,
            "pending state should clear after match"
        );
    }

    #[tokio::test]
    async fn choice_state_free_text_match_by_label() {
        use crate::messages::{ChoiceControl, ChoiceOption};

        let s = state(config_with_two_agents());
        let ctrl = ChoiceControl::new(
            "Pick agent",
            vec![
                ChoiceOption::agent("Librarian", "librarian"),
                ChoiceOption::agent("Critic", "critic"),
            ],
        );
        s.choice_state.record("mock", "brian", vec![ctrl]);

        let reply = route_mock_message(&s, "brian", "Critic")
            .await
            .expect("label reply should resolve");
        assert!(
            reply.contains("critic"),
            "reply should confirm switch to critic: {reply}"
        );
    }

    #[tokio::test]
    async fn choice_state_out_of_range_preserves_state() {
        use crate::messages::{ChoiceControl, ChoiceOption};

        let s = state(config_with_two_agents());
        let ctrl = ChoiceControl::new(
            "Pick agent",
            vec![
                ChoiceOption::agent("Librarian", "librarian"),
                ChoiceOption::agent("Critic", "critic"),
            ],
        );
        s.choice_state.record("mock", "brian", vec![ctrl]);

        let reply = route_mock_message(&s, "brian", "99")
            .await
            .expect("out-of-range should return a helpful message");
        assert!(
            reply.contains("isn't one of the options"),
            "should get out-of-range message: {reply}"
        );
        assert_eq!(
            s.choice_state.pending_len(),
            1,
            "state should be preserved for retry"
        );
    }

    #[tokio::test]
    async fn choice_state_ambiguous_preserves_state() {
        use crate::messages::{ChoiceControl, ChoiceOption};

        let s = state(config_with_two_agents());
        let ctrl = ChoiceControl::new(
            "Pick agent",
            vec![
                ChoiceOption::agent("Critic", "critic"),
                ChoiceOption::agent("Critique", "critique"),
            ],
        );
        s.choice_state.record("mock", "brian", vec![ctrl]);

        let reply = route_mock_message(&s, "brian", "Cri")
            .await
            .expect("ambiguous reply should return a helpful message");
        assert!(
            reply.contains("Multiple options match"),
            "should get ambiguity message: {reply}"
        );
        assert_eq!(
            s.choice_state.pending_len(),
            1,
            "state should be preserved for retry"
        );
    }

    #[tokio::test]
    async fn choice_state_no_pending_falls_through_to_normal_dispatch() {
        let s = state(config_with_mock_identity());
        assert_eq!(s.choice_state.pending_len(), 0);

        let reply = route_mock_message(&s, "brian", "hello world")
            .await
            .expect("should fall through to agent dispatch");
        assert!(
            reply.contains("hello world"),
            "echo agent should echo back: {reply}"
        );
    }

    #[tokio::test]
    async fn choice_state_cross_channel_keys_dont_collide() {
        use crate::messages::{ChoiceControl, ChoiceOption};

        let s = state(config_with_two_agents());
        let ctrl = ChoiceControl::new(
            "Pick",
            vec![
                ChoiceOption::agent("Librarian", "librarian"),
                ChoiceOption::agent("Critic", "critic"),
            ],
        );
        // Record on signal, not mock — mock should not see it.
        s.choice_state.record("signal", "brian", vec![ctrl]);

        let reply = route_mock_message(&s, "brian", "1")
            .await
            .expect("should fall through to agent dispatch, not match signal's pending");
        // "1" should go through as freeform text to the echo agent
        assert!(
            reply.contains("1"),
            "freeform '1' should reach echo agent: {reply}"
        );
        assert_eq!(
            s.choice_state.pending_len(),
            1,
            "signal pending should remain"
        );
    }

    #[tokio::test]
    async fn mock_api_accepts_identity_id_for_local_operator_tests() {
        let state = state(config_with_mock_identity());

        let response = route_mock_message(&state, "brian", "!status")
            .await
            .expect("identity id should be accepted on localhost mock API");

        assert!(
            response.contains("active agent: echo"),
            "status should be evaluated with brian's routing context: {response}"
        );
    }
}
