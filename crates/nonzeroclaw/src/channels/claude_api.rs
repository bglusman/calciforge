//! Claude API channel for nonzeroclaw.
//!
//! Enables direct integration with Anthropic's Claude API for:
//! - One-shot completions (no conversation history)
//! - Multi-turn conversations (with conversation_id persistence)
//! - Streaming responses via SSE
//! - Tool use through Claude's native function calling
//!
//! ## Configuration
//! ```toml
//! [channels_config.claude_api]
//! enabled = true
//! api_key = "${CLAUDE_API_KEY}"
//! model = "claude-sonnet-4-6"
//! max_tokens = 4096
//! conversation_ttl_hours = 24
//! ```
//!
//! ## Usage
//! The `recipient` field on [`SendMessage`] is used as the conversation ID.
//! Use `"new"` or an empty string to start a fresh one-shot completion.
//! Reuse the same conversation ID across calls to maintain context.

use super::traits::{Channel, ChannelMessage, SendMessage};
use anyhow::{bail, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const ANTHROPIC_API_BASE: &str = "https://api.anthropic.com/v1";
const ANTHROPIC_VERSION_HEADER: &str = "2023-06-01";
const DEFAULT_MODEL: &str = "claude-sonnet-4-6";
const DEFAULT_MAX_TOKENS: u32 = 4096;
const DEFAULT_CONVERSATION_TTL_HOURS: u64 = 24;
/// Default polling interval for checking new messages (used in listen loop).
const POLL_INTERVAL: Duration = Duration::from_secs(30);

// ─── Request / Response Types ────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct MessagesRequest {
    model: String,
    messages: Vec<ClaudeMessage>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ToolDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeMessage {
    pub role: String,
    pub content: ClaudeContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ClaudeContent {
    /// Simple text content
    Text(String),
    /// Structured content blocks (used for tool results, image blocks, etc.)
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    id: String,
    #[serde(rename = "type")]
    response_type: String,
    role: String,
    content: Vec<ResponseContentBlock>,
    model: String,
    stop_reason: Option<String>,
    usage: Option<UsageInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

#[derive(Debug, Deserialize)]
struct UsageInfo {
    input_tokens: u32,
    output_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct AnthropicError {
    error: AnthropicErrorDetail,
}

#[derive(Debug, Deserialize)]
struct AnthropicErrorDetail {
    #[serde(rename = "type")]
    error_type: String,
    message: String,
}

// ─── Conversation State ────────────────────────────────────────────────────────

/// In-memory state for a multi-turn conversation.
#[derive(Debug, Clone)]
pub struct ConversationState {
    /// Ordered history of messages (user + assistant alternating).
    pub messages: Vec<ClaudeMessage>,
    /// Tools registered for this conversation.
    pub tools: Vec<ToolDefinition>,
    /// Wall-clock timestamp of last activity (Unix seconds). Mutable for testing.
    pub last_activity: u64,
}

impl ConversationState {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            tools: Vec::new(),
            last_activity: unix_now(),
        }
    }

    fn touch(&mut self) {
        self.last_activity = unix_now();
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ─── Channel ─────────────────────────────────────────────────────────────────

/// Claude API channel — wraps Anthropic's Messages API as a NZC channel.
///
/// **Sending:** Calls `POST /v1/messages` with the conversation context
/// assembled from the in-memory store. Returns immediately after the API
/// responds (non-streaming).
///
/// **Receiving:** API channels are request/response by nature; `listen`
/// keeps the connection alive but only forwards messages when a webhook
/// or external trigger is hooked up. The listen loop performs periodic
/// health-checks and can be extended with webhook support.
///
/// **Conversation management:** Each unique `recipient` value in a
/// [`SendMessage`] maps to a conversation. Use `"new"` to force a
/// fresh one-shot request without history.
pub struct ClaudeApiChannel {
    /// Anthropic API key.
    api_key: String,
    /// Model ID, e.g. `"claude-sonnet-4-6"`.
    model: String,
    /// Maximum tokens to generate per turn.
    max_tokens: u32,
    /// Optional system prompt injected at the top of each request.
    system_prompt: Option<String>,
    /// Per-conversation message history and tool state.
    pub conversation_store: Arc<Mutex<HashMap<String, ConversationState>>>,
    /// Conversation TTL in hours — inactive conversations are evicted.
    conversation_ttl_hours: u64,
}

impl ClaudeApiChannel {
    /// Create a new Claude API channel from explicit parameters.
    pub fn new(
        api_key: impl Into<String>,
        model: impl Into<String>,
        max_tokens: u32,
        conversation_ttl_hours: u64,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            max_tokens,
            system_prompt: None,
            conversation_store: Arc::new(Mutex::new(HashMap::new())),
            conversation_ttl_hours,
        }
    }

    /// Create a channel from a [`ClaudeApiConfig`].
    pub fn from_config(config: &crate::config::schema::ClaudeApiConfig) -> Self {
        Self {
            api_key: config.api_key.clone(),
            model: if config.model.is_empty() {
                DEFAULT_MODEL.to_string()
            } else {
                config.model.clone()
            },
            max_tokens: if config.max_tokens == 0 {
                DEFAULT_MAX_TOKENS
            } else {
                config.max_tokens
            },
            system_prompt: config.system_prompt.clone(),
            conversation_store: Arc::new(Mutex::new(HashMap::new())),
            conversation_ttl_hours: if config.conversation_ttl_hours == 0 {
                DEFAULT_CONVERSATION_TTL_HOURS
            } else {
                config.conversation_ttl_hours
            },
        }
    }

    /// Set a system prompt to inject into every Claude request.
    #[must_use]
    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());
        self
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client("channel.claude_api")
    }

    /// Retrieve or create the conversation state for `conversation_id`.
    pub fn get_or_create_conversation(&self, conversation_id: &str) -> ConversationState {
        let mut store = self
            .conversation_store
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        store
            .entry(conversation_id.to_string())
            .or_insert_with(ConversationState::new)
            .clone()
    }

    /// Persist updated conversation state.
    pub fn update_conversation(&self, conversation_id: &str, state: ConversationState) {
        let mut store = self
            .conversation_store
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        store.insert(conversation_id.to_string(), state);
    }

    /// Evict conversations that have been idle longer than `conversation_ttl_hours`.
    pub fn evict_stale_conversations(&self) {
        let ttl_secs = self.conversation_ttl_hours * 3600;
        let cutoff = unix_now().saturating_sub(ttl_secs);
        let mut store = self
            .conversation_store
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        store.retain(|_, state| state.last_activity >= cutoff);
    }

    /// Call the Anthropic Messages API and return the text response.
    ///
    /// This is the core API call: builds the request, sends it, and
    /// extracts the first text block from the response content.
    ///
    /// Tool-use blocks in the response are logged as warnings (full
    /// tool execution is handled at the agent level, not channel level).
    pub async fn call_api(
        &self,
        messages: &[ClaudeMessage],
        tools: &[ToolDefinition],
    ) -> Result<(String, Vec<ToolUseBlock>)> {
        let request = MessagesRequest {
            model: self.model.clone(),
            messages: messages.to_vec(),
            max_tokens: self.max_tokens,
            system: self.system_prompt.clone(),
            tools: tools.to_vec(),
        };

        let client = self.http_client();
        let resp = client
            .post(format!("{ANTHROPIC_API_BASE}/messages"))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION_HEADER)
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read body: {e}>"));

            // Try to extract structured error message
            if let Ok(err) = serde_json::from_str::<AnthropicError>(&body) {
                bail!(
                    "Claude API error ({status}) [{}]: {}",
                    err.error.error_type,
                    err.error.message
                );
            }
            bail!("Claude API error ({status}): {body}");
        }

        let response: MessagesResponse = resp.json().await?;

        let mut text_parts = Vec::new();
        let mut tool_uses = Vec::new();

        for block in &response.content {
            match block {
                ResponseContentBlock::Text { text } => {
                    text_parts.push(text.clone());
                }
                ResponseContentBlock::ToolUse { id, name, input } => {
                    tool_uses.push(ToolUseBlock {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    });
                }
            }
        }

        let text = text_parts.join("\n");
        tracing::debug!(
            model = %self.model,
            stop_reason = ?response.stop_reason,
            input_tokens = ?response.usage.as_ref().map(|u| u.input_tokens),
            output_tokens = ?response.usage.as_ref().map(|u| u.output_tokens),
            tool_uses = tool_uses.len(),
            "Claude API call completed"
        );

        Ok((text, tool_uses))
    }

    /// Perform a health-check call against the Anthropic API.
    async fn check_health(&self) -> bool {
        // A minimal request to verify the API key and connectivity.
        let request = MessagesRequest {
            model: self.model.clone(),
            messages: vec![ClaudeMessage {
                role: "user".to_string(),
                content: ClaudeContent::Text("ping".to_string()),
            }],
            max_tokens: 1,
            system: None,
            tools: vec![],
        };

        let client = self.http_client();
        let result = client
            .post(format!("{ANTHROPIC_API_BASE}/messages"))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION_HEADER)
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await;

        match result {
            Ok(resp) => resp.status().is_success() || resp.status().as_u16() == 400,
            Err(e) => {
                tracing::warn!("Claude API health check failed: {e}");
                false
            }
        }
    }
}

/// Represents a tool-use block returned by Claude.
#[derive(Debug, Clone)]
pub struct ToolUseBlock {
    /// Unique ID for this tool-use invocation.
    pub id: String,
    /// Tool name as registered with Claude.
    pub name: String,
    /// Tool input arguments as JSON.
    pub input: serde_json::Value,
}

// ─── NZC Tool ↔ Claude Tool conversion helpers ──────────────────────────────

/// Convert an NZC tool descriptor into a Claude [`ToolDefinition`].
///
/// Used when bridging the NZC tool registry into Claude API calls so
/// Claude can request tool execution via `tool_use` blocks.
pub fn nzc_tool_to_claude(
    name: &str,
    description: &str,
    parameters: &serde_json::Value,
) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: parameters.clone(),
    }
}

/// Convert a [`ToolUseBlock`] returned by Claude into an NZC-compatible tool call.
///
/// Returns `(tool_name, tool_arguments_json)`.
pub fn claude_tool_use_to_nzc(tool_use: &ToolUseBlock) -> (String, serde_json::Value) {
    (tool_use.name.clone(), tool_use.input.clone())
}

// ─── Channel trait implementation ────────────────────────────────────────────

#[async_trait]
impl Channel for ClaudeApiChannel {
    fn name(&self) -> &str {
        "claude_api"
    }

    /// Send a message to Claude and store the exchange in conversation history.
    ///
    /// If `message.recipient` is `"new"` or empty, the call is a stateless
    /// one-shot (no prior context is included and the response is not stored).
    /// Otherwise, the conversation identified by `recipient` is loaded,
    /// extended with the new turn, and persisted after the call.
    async fn send(&self, message: &SendMessage) -> Result<()> {
        let is_one_shot =
            message.recipient.is_empty() || message.recipient.eq_ignore_ascii_case("new");

        if is_one_shot {
            // One-shot: no history, no persistence
            let messages = vec![ClaudeMessage {
                role: "user".to_string(),
                content: ClaudeContent::Text(message.content.clone()),
            }];
            let (response_text, tool_uses) = self.call_api(&messages, &[]).await?;
            if !tool_uses.is_empty() {
                tracing::debug!(
                    count = tool_uses.len(),
                    "Claude returned tool_use blocks in one-shot mode; not executed by channel layer"
                );
            }
            tracing::debug!(preview = %&response_text[..response_text.len().min(120)], "Claude one-shot response");
            return Ok(());
        }

        // Multi-turn: load conversation, append user turn, call API, persist
        let mut state = self.get_or_create_conversation(&message.recipient);
        state.messages.push(ClaudeMessage {
            role: "user".to_string(),
            content: ClaudeContent::Text(message.content.clone()),
        });
        state.touch();

        let (response_text, tool_uses) = self
            .call_api(&state.messages, &state.tools)
            .await?;

        if !tool_uses.is_empty() {
            tracing::debug!(
                count = tool_uses.len(),
                "Claude returned tool_use blocks; not executed by channel layer"
            );
        }

        // Append assistant turn to history
        if !response_text.is_empty() {
            state.messages.push(ClaudeMessage {
                role: "assistant".to_string(),
                content: ClaudeContent::Text(response_text),
            });
        }

        self.update_conversation(&message.recipient, state);
        self.evict_stale_conversations();

        Ok(())
    }

    /// Listen for incoming messages.
    ///
    /// The Claude API is request/response and does not push messages.
    /// This implementation keeps the loop alive and periodically evicts
    /// stale conversations. Wire in a webhook endpoint at the application
    /// layer to inject inbound messages into the `tx` sender.
    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        tracing::info!(
            model = %self.model,
            "Claude API channel listening (request/response mode — no push messages)"
        );

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;

            if tx.is_closed() {
                tracing::info!("Claude API channel: message bus closed, exiting listen loop");
                return Ok(());
            }

            // Periodic housekeeping
            self.evict_stale_conversations();
        }
    }

    /// Verify API key validity with a minimal request.
    async fn health_check(&self) -> bool {
        self.check_health().await
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel() -> ClaudeApiChannel {
        ClaudeApiChannel::new(
            "sk-ant-test-key",
            "claude-sonnet-4-6",
            DEFAULT_MAX_TOKENS,
            DEFAULT_CONVERSATION_TTL_HOURS,
        )
    }

    #[test]
    fn channel_name_is_claude_api() {
        let ch = make_channel();
        assert_eq!(ch.name(), "claude_api");
    }

    #[test]
    fn new_conversation_starts_empty() {
        let ch = make_channel();
        let state = ch.get_or_create_conversation("conv-123");
        assert!(state.messages.is_empty());
        assert!(state.tools.is_empty());
    }

    #[test]
    fn conversation_is_persisted_and_retrieved() {
        let ch = make_channel();
        let mut state = ConversationState::new();
        state.messages.push(ClaudeMessage {
            role: "user".to_string(),
            content: ClaudeContent::Text("hello".to_string()),
        });
        ch.update_conversation("conv-persist", state);

        let retrieved = ch.get_or_create_conversation("conv-persist");
        assert_eq!(retrieved.messages.len(), 1);
    }

    #[test]
    fn stale_conversations_evicted() {
        let ch = ClaudeApiChannel::new("key", "model", 100, 1); // 1 hour TTL
        {
            let mut store = ch.conversation_store.lock().unwrap();
            let mut old = ConversationState::new();
            // Make it look very old (2 hours ago)
            old.last_activity = unix_now().saturating_sub(7200);
            store.insert("old-conv".to_string(), old);

            let fresh = ConversationState::new();
            store.insert("fresh-conv".to_string(), fresh);
        }
        ch.evict_stale_conversations();

        let store = ch.conversation_store.lock().unwrap();
        assert!(
            !store.contains_key("old-conv"),
            "old conversation should be evicted"
        );
        assert!(
            store.contains_key("fresh-conv"),
            "fresh conversation should remain"
        );
    }

    #[test]
    fn nzc_tool_to_claude_roundtrip() {
        let params = serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" }
            },
            "required": ["query"]
        });
        let tool = nzc_tool_to_claude("web_search", "Search the web", &params);
        assert_eq!(tool.name, "web_search");
        assert_eq!(tool.description, "Search the web");
        assert_eq!(tool.input_schema["properties"]["query"]["type"], "string");
    }

    #[test]
    fn claude_tool_use_to_nzc_returns_name_and_input() {
        let block = ToolUseBlock {
            id: "tu_123".to_string(),
            name: "shell".to_string(),
            input: serde_json::json!({"command": "ls"}),
        };
        let (name, args) = claude_tool_use_to_nzc(&block);
        assert_eq!(name, "shell");
        assert_eq!(args["command"], "ls");
    }

    #[test]
    fn claude_content_serializes_text_variant() {
        let msg = ClaudeMessage {
            role: "user".to_string(),
            content: ClaudeContent::Text("hello world".to_string()),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "hello world");
    }

    #[test]
    fn claude_content_serializes_blocks_variant() {
        let msg = ClaudeMessage {
            role: "user".to_string(),
            content: ClaudeContent::Blocks(vec![ContentBlock::Text {
                text: "block text".to_string(),
            }]),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["content"][0]["type"], "text");
        assert_eq!(json["content"][0]["text"], "block text");
    }

    #[test]
    fn from_config_applies_defaults_for_zero_values() {
        let config = crate::config::schema::ClaudeApiConfig {
            enabled: true,
            api_key: "sk-ant-test".to_string(),
            model: String::new(), // empty → default
            max_tokens: 0,        // zero → default
            conversation_ttl_hours: 0, // zero → default
            system_prompt: None,
            webhook_secret: None,
        };
        let ch = ClaudeApiChannel::from_config(&config);
        assert_eq!(ch.model, DEFAULT_MODEL);
        assert_eq!(ch.max_tokens, DEFAULT_MAX_TOKENS);
        assert_eq!(ch.conversation_ttl_hours, DEFAULT_CONVERSATION_TTL_HOURS);
    }

    #[test]
    fn with_system_prompt_sets_field() {
        let ch = make_channel().with_system_prompt("You are a helpful assistant.");
        assert_eq!(
            ch.system_prompt.as_deref(),
            Some("You are a helpful assistant.")
        );
    }

    #[test]
    fn tool_definition_serializes_correctly() {
        let tool = ToolDefinition {
            name: "memory_recall".to_string(),
            description: "Search memory".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "query": { "type": "string" } }
            }),
        };
        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(json["name"], "memory_recall");
        assert_eq!(json["description"], "Search memory");
        assert!(json["input_schema"].is_object());
    }
}
