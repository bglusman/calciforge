//! Text/iMessage channel adapter for Calciforge.
//!
//! Calciforge exposes `kind = "sms"` and uses `zeroclawlabs::LinqChannel`
//! underneath. Linq is webhook based for inbound iMessage/RCS/SMS events, so
//! this module hosts a small webhook receiver, lets the zeroclawlabs parser
//! normalize incoming payloads, then sends replies through the same `Channel`
//! interface used by other embedded transports.

use crate::sync::Arc;
use anyhow::{Context, Result, bail};
use axum::{
    Json, Router as AxumRouter,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose};
use hmac::{Hmac, Mac};
use serde_json::json;
use sha1::Sha1;
use std::time::Duration;
use tracing::{debug, info, warn};
use url::form_urlencoded;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_channels::linq::LinqChannel as ZclLinqChannel;

use crate::{
    auth::{find_agent, resolve_channel_sender},
    commands::CommandHandler,
    config::{CalciforgeConfig, expand_tilde},
    context::ContextStore,
    messages::OutboundMessage,
    router::Router,
};

use super::{runtime, telemetry};

use adversary_detector::middleware::ChannelScanner;

type HmacSha1 = Hmac<Sha1>;

pub struct SmsChannel<C: Channel + ?Sized = ZclLinqChannel> {
    config: Arc<CalciforgeConfig>,
    router: Arc<Router>,
    command_handler: Arc<CommandHandler>,
    context_store: ContextStore,
    channel_scanner: Arc<ChannelScanner>,
    transport: Arc<C>,
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
        }
    }

    fn scan_enabled(&self) -> bool {
        runtime::scan_enabled(&self.config, "sms")
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

    async fn send_outbound(&self, recipient: &str, message: &OutboundMessage) {
        self.send_reply(recipient, &message.render_text_fallback())
            .await;
    }

    pub async fn handle_message(self: Arc<Self>, msg: ChannelMessage) {
        let received_at = std::time::Instant::now();
        let delivery_lag_ms = telemetry::delivery_lag_ms_from_unix_seconds(msg.timestamp);

        let from = msg.sender.clone();
        let reply_target = runtime::reply_target(&msg);
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

        if self.scan_enabled()
            && let Some(reply) = runtime::inbound_scan_block_reply(
                "sms",
                "Text/iMessage",
                &identity.id,
                &text,
                &self.channel_scanner,
                "Message blocked by security scanner",
            )
            .await
        {
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        let text = match self
            .command_handler
            .resolve_pending_choice_reply(&identity.id, &text)
        {
            Some(crate::commands::PendingChoiceReply::Command(command)) => command,
            Some(crate::commands::PendingChoiceReply::Reply(reply)) => {
                let channel = self.clone();
                let target = reply_target.clone();
                tokio::spawn(async move {
                    channel.send_reply(&target, &reply).await;
                });
                return;
            }
            None => text,
        };

        if let Some(reply) = self
            .command_handler
            .agent_choice_message_for_identity(&text, &identity.id)
        {
            self.command_handler
                .record_pending_choices(&identity.id, &reply);
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_outbound(&target, &reply).await;
            });
            return;
        }

        if let Some(reply) = self.command_handler.model_choice_message(&text) {
            self.command_handler
                .record_pending_choices(&identity.id, &reply);
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_outbound(&target, &reply).await;
            });
            return;
        }

        if let Some(reply) = self.command_handler.handle(&text) {
            debug!(identity = %identity.id, cmd = %text.trim(), "Text/iMessage: handled identity-resolved local command");
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        if CommandHandler::is_unknown_channel_command(&text) {
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

        if CommandHandler::is_gateway_command(&text) {
            let reply = self.command_handler.cmd_gateway_for_identity(&identity.id);
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
            self.command_handler
                .record_pending_choices(&identity.id, &reply);
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_outbound(&target, &reply).await;
            });
            return;
        }

        if CommandHandler::is_new_session_command(&text) {
            let reply = self.command_handler.handle_new_session(&text, &identity.id);
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
            });
            return;
        }

        if CommandHandler::is_btw_command(&text) {
            let reply = match self.command_handler.parse_btw_command(&text, &identity.id) {
                Ok(request) => {
                    let model_override =
                        self.command_handler.active_model_for_identity(&identity.id);
                    let dispatch_start = std::time::Instant::now();
                    match self
                        .router
                        .dispatch_one_off_for_identity(
                            &request.prompt,
                            &request.agent_id,
                            &self.config,
                            &identity.id,
                            "sms",
                            model_override.as_deref(),
                        )
                        .await
                    {
                        Ok(response) => {
                            self.command_handler
                                .record_dispatch(dispatch_start.elapsed().as_millis() as u64);
                            format!("{}:\n{}", request.agent_id, response.render_text_fallback())
                        }
                        Err(err) => format!("⚠️ !btw dispatch failed: {err}"),
                    }
                }
                Err(err) => err,
            };
            let channel = self.clone();
            let target = reply_target.clone();
            tokio::spawn(async move {
                channel.send_reply(&target, &reply).await;
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

        if CommandHandler::is_approve_command(&text) || CommandHandler::is_deny_command(&text) {
            if let Some((ack, follow_up)) = self.command_handler.handle_async(&text).await {
                let channel = self.clone();
                let target = reply_target.clone();
                tokio::spawn(async move {
                    channel.send_reply(&target, &ack).await;
                    if let Some(follow_up) = follow_up {
                        channel.send_reply(&target, &follow_up).await;
                    }
                });
            }
            return;
        }

        if CommandHandler::is_context_clear_command(&text) {
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
                            "Text/iMessage: clash approval request - forwarding to user"
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
                        self.command_handler
                            .record_pending_choices(&identity_id, &notification);
                        self.send_outbound(&reply_target, &notification).await;
                        return;
                    }
                    warn!(identity = %identity_id, error = %e, "Text/iMessage: agent dispatch failed");
                    self.send_reply(&reply_target, &format!("Agent error: {e}"))
                        .await;
                }
            }
        });
    }
}

fn conversation_chat_key(identity_id: &str, reply_target: &str) -> String {
    format!("sms-{identity_id}-{reply_target}")
}

#[derive(Clone)]
struct TwilioChannel {
    account_sid: String,
    auth_token: String,
    from: Option<String>,
    messaging_service_sid: Option<String>,
    allowed_senders: Vec<String>,
    api_base: String,
    client: reqwest::Client,
}

impl TwilioChannel {
    fn new(
        account_sid: String,
        auth_token: String,
        from: Option<String>,
        messaging_service_sid: Option<String>,
        allowed_senders: Vec<String>,
    ) -> Self {
        Self {
            account_sid,
            auth_token,
            from,
            messaging_service_sid,
            allowed_senders,
            api_base: "https://api.twilio.com".to_string(),
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(20))
                .build()
                .expect("building Twilio SMS/RCS HTTP client"),
        }
    }

    #[cfg(test)]
    fn with_api_base(mut self, api_base: String) -> Self {
        self.api_base = api_base.trim_end_matches('/').to_string();
        self
    }

    fn is_sender_allowed(&self, sender: &str) -> bool {
        self.allowed_senders
            .iter()
            .any(|allowed| allowed == "*" || allowed == sender)
    }

    fn parse_webhook_form(&self, params: &[(String, String)]) -> Vec<ChannelMessage> {
        let value = |name: &str| {
            params
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };

        let Some(from) = value("From").map(normalize_twilio_sender) else {
            return Vec::new();
        };

        if !self.is_sender_allowed(&from) {
            warn!(from = %from, "Twilio SMS/RCS: unauthorized sender - dropping");
            return Vec::new();
        }

        let button_payload = value("ButtonPayload")
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let button_text = value("ButtonText").map(str::trim).filter(|v| !v.is_empty());
        let body = value("Body").map(str::trim).filter(|v| !v.is_empty());
        let mut content_parts = Vec::new();
        if let Some(content) = button_payload.or(body).or(button_text) {
            content_parts.push(content.to_string());
        }

        let media_count = value("NumMedia")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        for index in 0..media_count {
            let media_url_key = format!("MediaUrl{index}");
            let media_type_key = format!("MediaContentType{index}");
            let Some(media_url) = value(&media_url_key)
                .map(str::trim)
                .filter(|v| !v.is_empty())
            else {
                continue;
            };
            let media_type = value(&media_type_key)
                .map(str::trim)
                .unwrap_or("application/octet-stream");
            if media_type.to_ascii_lowercase().starts_with("image/") {
                content_parts.push(format!("[IMAGE:{media_url}]"));
            } else {
                content_parts.push(format!("[MEDIA:{media_type}:{media_url}]"));
            }
        }

        let content = content_parts.join("\n").trim().to_string();
        if content.is_empty() {
            return Vec::new();
        }

        let id = value("MessageSid")
            .or_else(|| value("SmsMessageSid"))
            .or_else(|| value("SmsSid"))
            .unwrap_or("twilio-message")
            .to_string();
        vec![ChannelMessage {
            id,
            sender: from.clone(),
            reply_target: from,
            content,
            channel: "sms".to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        }]
    }

    fn verify_signature(
        auth_token: &str,
        signature: &str,
        public_url: &str,
        params: &[(String, String)],
    ) -> bool {
        let Ok(mut mac) = HmacSha1::new_from_slice(auth_token.as_bytes()) else {
            return false;
        };
        mac.update(twilio_signature_base(public_url, params).as_bytes());
        let Ok(signature_bytes) = general_purpose::STANDARD.decode(signature.trim()) else {
            return false;
        };
        mac.verify_slice(&signature_bytes).is_ok()
    }
}

#[async_trait::async_trait]
impl Channel for TwilioChannel {
    fn name(&self) -> &str {
        "twilio-sms"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let url = format!(
            "{}/2010-04-01/Accounts/{}/Messages.json",
            self.api_base, self.account_sid
        );
        let mut form = vec![
            ("To".to_string(), message.recipient.clone()),
            ("Body".to_string(), message.content.clone()),
        ];
        if let Some(service_sid) = self.messaging_service_sid.as_deref() {
            form.push(("MessagingServiceSid".to_string(), service_sid.to_string()));
        } else if let Some(from) = self.from.as_deref() {
            form.push(("From".to_string(), from.to_string()));
        } else {
            bail!("Twilio SMS/RCS requires sms_from_phone or sms_twilio_messaging_service_sid");
        }

        let response = self
            .client
            .post(url)
            .basic_auth(&self.account_sid, Some(&self.auth_token))
            .form(&form)
            .send()
            .await
            .context("Twilio SMS/RCS send request failed")?;

        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Twilio SMS/RCS send failed ({status}): {body}");
        }
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        Err(anyhow::anyhow!(
            "Twilio SMS/RCS is webhook-driven; Calciforge hosts the listener"
        ))
    }
}

fn normalize_twilio_sender(sender: &str) -> String {
    let trimmed = sender.trim();
    let without_prefix = trimmed
        .strip_prefix("sms:")
        .or_else(|| trimmed.strip_prefix("rcs:"))
        .unwrap_or(trimmed);
    if without_prefix.starts_with('+') || without_prefix.contains(':') {
        without_prefix.to_string()
    } else {
        format!("+{without_prefix}")
    }
}

fn twilio_form_params(body: &[u8]) -> Option<Vec<(String, String)>> {
    std::str::from_utf8(body).ok().map(|body| {
        form_urlencoded::parse(body.as_bytes())
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect()
    })
}

fn twilio_signature_base(public_url: &str, params: &[(String, String)]) -> String {
    let mut sorted = params.to_vec();
    sorted.sort_by(|(key_a, value_a), (key_b, value_b)| {
        key_a.cmp(key_b).then_with(|| value_a.cmp(value_b))
    });

    let mut base = public_url.to_string();
    for (key, value) in sorted {
        base.push_str(&key);
        base.push_str(&value);
    }
    base
}

#[derive(Clone)]
struct WebhookState {
    bridge: Arc<SmsChannel<ZclLinqChannel>>,
    transport: Arc<ZclLinqChannel>,
    signing_secret: Option<String>,
}

#[derive(Clone)]
struct TwilioWebhookState {
    bridge: Arc<SmsChannel<TwilioChannel>>,
    transport: Arc<TwilioChannel>,
    auth_token: String,
    public_url: Option<String>,
    disable_signature_validation: bool,
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

async fn twilio_webhook_handler(
    State(state): State<TwilioWebhookState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let Some(params) = twilio_form_params(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            "body must be x-www-form-urlencoded",
        );
    };

    if !state.disable_signature_validation {
        let Some(public_url) = state.public_url.as_deref() else {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "missing public webhook URL",
            );
        };
        let signature = match headers
            .get("x-twilio-signature")
            .and_then(|value| value.to_str().ok())
        {
            Some(value) => value,
            None => return (StatusCode::UNAUTHORIZED, "missing Twilio signature"),
        };
        if !TwilioChannel::verify_signature(&state.auth_token, signature, public_url, &params) {
            return (StatusCode::UNAUTHORIZED, "invalid Twilio signature");
        }
    }

    let messages = state.transport.parse_webhook_form(&params);
    for msg in messages {
        let bridge = state.bridge.clone();
        tokio::spawn(async move {
            bridge.handle_message(msg).await;
        });
    }

    (StatusCode::OK, "<Response></Response>")
}

fn read_secret_file(path: &str, label: &str) -> Result<String> {
    Ok(std::fs::read_to_string(expand_tilde(path))
        .with_context(|| format!("SMS channel: failed to read {label} '{path}'"))?
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
) -> Result<()> {
    let sms_cfg = config
        .channels
        .iter()
        .find(|c| c.kind == "sms" && c.enabled)
        .context("no enabled sms channel found in config")?;

    match sms_cfg
        .sms_provider
        .as_deref()
        .unwrap_or("linq")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "linq" => {
            run_linq(
                config,
                router,
                command_handler,
                context_store,
                channel_scanner,
            )
            .await
        }
        "twilio" => {
            run_twilio(
                config,
                router,
                command_handler,
                context_store,
                channel_scanner,
            )
            .await
        }
        provider => bail!("unsupported sms_provider '{provider}'; expected 'linq' or 'twilio'"),
    }
}

async fn run_linq(
    config: Arc<CalciforgeConfig>,
    router: Arc<Router>,
    command_handler: Arc<CommandHandler>,
    context_store: ContextStore,
    channel_scanner: Arc<ChannelScanner>,
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
    let bridge = Arc::new(SmsChannel::<ZclLinqChannel>::new(
        config,
        router,
        command_handler,
        context_store,
        channel_scanner,
        transport.clone(),
    ));

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

async fn run_twilio(
    config: Arc<CalciforgeConfig>,
    router: Arc<Router>,
    command_handler: Arc<CommandHandler>,
    context_store: ContextStore,
    channel_scanner: Arc<ChannelScanner>,
) -> Result<()> {
    let sms_cfg = config
        .channels
        .iter()
        .find(|c| c.kind == "sms" && c.enabled)
        .context("no enabled sms channel found in config")?;

    let account_sid = resolve_optional_secret(
        &sms_cfg.sms_twilio_account_sid,
        &sms_cfg.sms_twilio_account_sid_file,
        "sms_twilio_account_sid_file",
    )?
    .filter(|value| !value.is_empty())
    .context(
        "sms_twilio_account_sid_file or sms_twilio_account_sid is required when sms_provider = \"twilio\"",
    )?;
    let auth_token = resolve_optional_secret(
        &sms_cfg.sms_twilio_auth_token,
        &sms_cfg.sms_twilio_auth_token_file,
        "sms_twilio_auth_token_file",
    )?
    .filter(|value| !value.is_empty())
    .context(
        "sms_twilio_auth_token_file or sms_twilio_auth_token is required when sms_provider = \"twilio\"",
    )?;
    let from = sms_cfg
        .sms_from_phone
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let messaging_service_sid = sms_cfg
        .sms_twilio_messaging_service_sid
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    if from.is_none() && messaging_service_sid.is_none() {
        bail!("Twilio SMS/RCS requires sms_from_phone or sms_twilio_messaging_service_sid");
    }

    let public_url = sms_cfg.sms_twilio_webhook_public_url.clone();
    let disable_signature_validation = sms_cfg.sms_twilio_disable_signature_validation;

    if !disable_signature_validation && public_url.is_none() {
        bail!(
            "Twilio SMS/RCS signature validation requires sms_twilio_webhook_public_url; \
             set sms_twilio_disable_signature_validation = true only for local test tunnels"
        );
    }
    if disable_signature_validation {
        warn!(
            "Twilio SMS/RCS webhook signature verification is disabled; \
             public endpoints should configure sms_twilio_webhook_public_url"
        );
    }

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
        signed = !disable_signature_validation,
        messaging_service = messaging_service_sid.is_some(),
        "Text/RCS channel starting (Twilio webhook receiver)"
    );

    let transport = Arc::new(TwilioChannel::new(
        account_sid,
        auth_token.clone(),
        from,
        messaging_service_sid,
        allowed,
    ));
    let bridge = Arc::new(SmsChannel::<TwilioChannel>::new(
        config,
        router,
        command_handler,
        context_store,
        channel_scanner,
        transport.clone(),
    ));
    let state = TwilioWebhookState {
        bridge,
        transport,
        auth_token,
        public_url,
        disable_signature_validation,
    };
    let app = AxumRouter::new()
        .route("/health", get(health_handler))
        .route(&webhook_path, post(twilio_webhook_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&listen_addr)
        .await
        .with_context(|| format!("binding Twilio SMS/RCS webhook listener on {listen_addr}"))?;

    axum::serve(listener, app)
        .await
        .context("Twilio SMS/RCS webhook listener exited")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AgentConfig, CalciforgeConfig, CalciforgeHeader, ChannelAlias, ChannelConfig, Identity,
        RoutingRule,
    };
    use async_trait::async_trait;
    use mockito::Matcher;
    use std::sync::Mutex as StdMutex;
    use tokio::sync::Notify;
    use tokio::sync::mpsc;

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
                btw_agent: None,
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
            model_roles: vec![],
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

    #[test]
    fn twilio_signature_validation_accepts_all_form_fields() {
        let params = vec![
            ("Body".to_string(), "hello".to_string()),
            ("From".to_string(), "+15555550100".to_string()),
            ("To".to_string(), "+15555550001".to_string()),
            ("MessageSid".to_string(), "SM123".to_string()),
            ("ButtonPayload".to_string(), "!approve req-1".to_string()),
        ];
        let public_url = "https://sms.example.test/webhooks/sms";
        let auth_token = "twilio-secret";
        let mut mac = HmacSha1::new_from_slice(auth_token.as_bytes()).unwrap();
        mac.update(twilio_signature_base(public_url, &params).as_bytes());
        let signature = general_purpose::STANDARD.encode(mac.finalize().into_bytes());

        assert!(TwilioChannel::verify_signature(
            auth_token, &signature, public_url, &params
        ));

        let mut tampered = params.clone();
        tampered.push(("UnexpectedFutureParam".to_string(), "value".to_string()));
        assert!(
            !TwilioChannel::verify_signature(auth_token, &signature, public_url, &tampered),
            "signature validation must include all current and future Twilio form fields"
        );
    }

    #[test]
    fn twilio_webhook_form_prefers_button_payload_and_media_markers() {
        let transport = TwilioChannel::new(
            "AC123".to_string(),
            "secret".to_string(),
            Some("+15555550001".to_string()),
            None,
            vec!["+15555550100".to_string()],
        );
        let body = b"MessageSid=SM123&From=%2B15555550100&To=%2B15555550001&Body=Approve&ButtonPayload=%21approve+req-1&NumMedia=1&MediaUrl0=https%3A%2F%2Fapi.twilio.com%2Fmedia%2F1&MediaContentType0=image%2Fjpeg";
        let params = twilio_form_params(body).expect("valid form body");

        let messages = transport.parse_webhook_form(&params);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].sender, "+15555550100");
        assert_eq!(messages[0].reply_target, "+15555550100");
        assert!(
            messages[0].content.starts_with("!approve req-1"),
            "RCS/WhatsApp button payloads should become Calciforge commands when present: {}",
            messages[0].content
        );
        assert!(
            messages[0]
                .content
                .contains("[IMAGE:https://api.twilio.com/media/1]"),
            "inbound media should survive as an artifact marker for the shared channel pipeline"
        );
    }

    #[test]
    fn twilio_webhook_form_normalizes_rcs_reply_targets_to_sms_identity_space() {
        let transport = TwilioChannel::new(
            "AC123".to_string(),
            "secret".to_string(),
            Some("+15555550001".to_string()),
            None,
            vec!["+15555550100".to_string()],
        );
        let params = twilio_form_params(b"MessageSid=SM123&From=rcs%3A%2B15555550100&Body=hello")
            .expect("valid form body");

        let messages = transport.parse_webhook_form(&params);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].sender, "+15555550100");
        assert_eq!(messages[0].reply_target, "+15555550100");
        assert_eq!(messages[0].channel, "sms");
    }

    #[test]
    fn twilio_webhook_form_rejects_unallowed_sender() {
        let transport = TwilioChannel::new(
            "AC123".to_string(),
            "secret".to_string(),
            Some("+15555550001".to_string()),
            None,
            vec!["+15555550100".to_string()],
        );
        let params =
            twilio_form_params(b"MessageSid=SM123&From=%2B15555559999&Body=hello&NumMedia=0")
                .expect("valid form body");

        assert!(
            transport.parse_webhook_form(&params).is_empty(),
            "Twilio inbound messages must use the same allowed_numbers gate as Linq SMS"
        );
    }

    #[tokio::test]
    async fn twilio_outbound_uses_messaging_service_when_configured() {
        let mut server = mockito::Server::new_async().await;
        let auth = format!(
            "Basic {}",
            general_purpose::STANDARD.encode("AC123:twilio-secret")
        );
        let send = server
            .mock("POST", "/2010-04-01/Accounts/AC123/Messages.json")
            .match_header("authorization", auth.as_str())
            .match_body(Matcher::AllOf(vec![
                Matcher::UrlEncoded("To".into(), "+15555550100".into()),
                Matcher::UrlEncoded("Body".into(), "hello from calciforge".into()),
                Matcher::UrlEncoded("MessagingServiceSid".into(), "MG123".into()),
            ]))
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(r#"{"sid":"SM123","status":"queued"}"#)
            .create_async()
            .await;
        let transport = TwilioChannel::new(
            "AC123".to_string(),
            "twilio-secret".to_string(),
            Some("+15555550001".to_string()),
            Some("MG123".to_string()),
            vec!["*".to_string()],
        )
        .with_api_base(server.url());

        transport
            .send(&SendMessage::new("hello from calciforge", "+15555550100"))
            .await
            .expect("Twilio send should succeed");

        send.assert_async().await;
    }
}
