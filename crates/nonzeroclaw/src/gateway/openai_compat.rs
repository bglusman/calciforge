//! OpenAI-compatible `/v1/chat/completions` endpoint for NonZeroClaw.
//!
//! This module implements the OpenAI Chat Completions API surface so that
//! PolyClaw (and any other OpenAI-compatible router) can route to NonZeroClaw
//! via the existing `openclaw-http` adapter.
//!
//! # Supported endpoints
//!
//! - `POST /v1/chat/completions` — Chat completions (streaming + non-streaming)
//! - `GET  /v1/models`           — Model list (returns a static stub)
//!
//! # Authentication
//!
//! If pairing is enabled, the bearer token from `Authorization: Bearer <token>`
//! is validated against the `PairingGuard`. When pairing is disabled, all
//! requests are accepted.
//!
//! # Outpost scanning
//!
//! Tool call results that originate from external sources (web fetch, exec,
//! browser) are passed through the `OutpostScanner` before being returned to
//! the model context.

use super::AppState;
use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Json,
    },
};
use outpost::{OutpostScanner, ScannerConfig};
use outpost::verdict::ScanContext;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;



// ── Request / Response types ──────────────────────────────────────────────────

/// A single message in the OpenAI chat format.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatCompletionMessage {
    /// Role: "system", "user", "assistant", or "tool".
    pub role: String,
    /// Message content (may be null for tool-call assistant turns).
    pub content: Option<String>,
    /// Tool call ID (for `role = "tool"` result messages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Tool call name (for `role = "tool"` result messages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// OpenAI-compatible `POST /v1/chat/completions` request body.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionRequest {
    /// Target model.  NonZeroClaw passes this through to the configured provider.
    pub model: Option<String>,
    /// Conversation history.
    pub messages: Vec<ChatCompletionMessage>,
    /// Sampling temperature (0.0–2.0).
    #[serde(default = "default_temperature")]
    pub temperature: f64,
    /// Maximum tokens to generate (optional).
    pub max_tokens: Option<u32>,
    /// Whether to stream the response via SSE.
    #[serde(default)]
    pub stream: bool,
}

fn default_temperature() -> f64 {
    0.7
}

/// A single completion choice.
#[derive(Debug, Serialize)]
pub struct Choice {
    pub index: u32,
    pub message: ChatCompletionMessage,
    pub finish_reason: String,
}

/// OpenAI-compatible `POST /v1/chat/completions` response body.
#[derive(Debug, Serialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
}

/// Delta for a streaming chunk choice.
#[derive(Debug, Serialize)]
pub struct ChunkDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// A single choice in a streaming chunk.
#[derive(Debug, Serialize)]
pub struct ChunkChoice {
    pub index: u32,
    pub delta: ChunkDelta,
    pub finish_reason: Option<String>,
}

/// OpenAI-compatible streaming chunk body.
#[derive(Debug, Serialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
}

/// Single model entry for `GET /v1/models`.
#[derive(Debug, Serialize)]
pub struct ModelEntry {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub owned_by: String,
}

/// Response body for `GET /v1/models`.
#[derive(Debug, Serialize)]
pub struct ModelsResponse {
    pub object: String,
    pub data: Vec<ModelEntry>,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Apply outpost scanning to a tool result before it returns to the model.
///
/// `url` is a best-effort source identifier for audit logging.
/// Content that is Unsafe is replaced with a blocking notice.
/// Content flagged for Review has a warning prepended.
async fn scan_tool_output(output: &str, url: &str, ctx: ScanContext) -> String {
    let scanner = OutpostScanner::new(ScannerConfig::default());
    let verdict = scanner.scan(url, output, ctx).await;
    match verdict {
        outpost::verdict::OutpostVerdict::Clean => output.to_string(),
        outpost::verdict::OutpostVerdict::Review { reason } => {
            format!("[OUTPOST REVIEW: {reason}]\n{output}")
        }
        outpost::verdict::OutpostVerdict::Unsafe { reason } => {
            format!("[OUTPOST BLOCKED: {reason}]")
        }
    }
}

/// Detect whether a tool result message looks like it came from an external
/// source (web fetch, exec, browser) and should be scanned by outpost.
///
/// Heuristic: tool result messages (role = "tool") whose content looks like
/// HTTP response bodies or JSON payloads are scanned.
fn detect_scan_context(msg: &ChatCompletionMessage) -> Option<ScanContext> {
    if msg.role != "tool" {
        return None;
    }
    let content = msg.content.as_deref().unwrap_or("");
    // Heuristics for common external tool outputs
    if content.contains("<!DOCTYPE") || content.contains("<html") {
        return Some(ScanContext::WebFetch);
    }
    if content.starts_with('{') || content.starts_with('[') {
        return Some(ScanContext::Api);
    }
    // Default for exec-style results
    Some(ScanContext::Exec)
}

/// Scan all tool result messages in the conversation before sending to the LLM.
///
/// This function is shared between the streaming and non-streaming paths to
/// ensure scanning is never bypassed.
async fn scan_messages(messages: &[ChatCompletionMessage]) -> Vec<ChatCompletionMessage> {
    let mut scanned = Vec::with_capacity(messages.len());
    for msg in messages {
        if let Some(ctx) = detect_scan_context(msg) {
            let raw = msg.content.as_deref().unwrap_or("");
            // Use tool_call_id or name as a proxy URL for audit logging
            let source_hint = msg
                .tool_call_id
                .as_deref()
                .or(msg.name.as_deref())
                .unwrap_or("tool_result");
            let scanned_content = scan_tool_output(raw, source_hint, ctx).await;
            scanned.push(ChatCompletionMessage {
                role: msg.role.clone(),
                content: Some(scanned_content),
                tool_call_id: msg.tool_call_id.clone(),
                name: msg.name.clone(),
            });
        } else {
            scanned.push(msg.clone());
        }
    }
    scanned
}

/// Inject AGENTS.md as system prompt if none is provided.
async fn inject_agents_md_if_needed(
    messages: &mut Vec<ChatCompletionMessage>,
    workspace_dir: &std::path::Path,
) {
    let has_system = messages.iter().any(|m| m.role == "system");
    if !has_system {
        match tokio::fs::read_to_string(workspace_dir.join("AGENTS.md")).await {
            Ok(agents_md) => {
                messages.insert(0, ChatCompletionMessage {
                    role: "system".to_string(),
                    content: Some(agents_md),
                    tool_call_id: None,
                    name: None,
                });
            }
            Err(_) => {
                tracing::warn!("AGENTS.md not found at {:?}, no system prompt injected", workspace_dir);
            }
        }
    }
}

/// Convert OpenAI-compat messages to provider ChatMessage format.
fn to_provider_messages(messages: &[ChatCompletionMessage]) -> Vec<crate::providers::ChatMessage> {
    messages
        .iter()
        .filter_map(|m| {
            let content = m.content.clone().unwrap_or_default();
            match m.role.as_str() {
                "system" => Some(crate::providers::ChatMessage::system(content)),
                "user" => Some(crate::providers::ChatMessage::user(content)),
                "assistant" => Some(crate::providers::ChatMessage::assistant(content)),
                "tool" => Some(crate::providers::ChatMessage::user(format!(
                    "[Tool result{}]: {}",
                    m.tool_call_id
                        .as_deref()
                        .map(|id| format!(" ({id})"))
                        .unwrap_or_default(),
                    content
                ))),
                _ => None,
            }
        })
        .collect()
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// `POST /v1/chat/completions` — OpenAI-compatible chat completions.
///
/// Accepts an OpenAI `ChatCompletionRequest`, runs tool results through the
/// outpost scanner, then forwards to the configured LLM provider.
/// Supports both streaming (SSE) and non-streaming responses.
pub async fn handle_chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<ChatCompletionRequest>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    // ── Auth ────────────────────────────────────────────────────────────
    if state.pairing.require_pairing() {
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|a| a.strip_prefix("Bearer "))
            .unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            let err = serde_json::json!({
                "error": {
                    "message": "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>",
                    "type": "authentication_error",
                    "code": "unauthorized"
                }
            });
            return (StatusCode::UNAUTHORIZED, Json(err)).into_response();
        }
    }

    // ── Parse body ──────────────────────────────────────────────────────
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("OpenAI-compat JSON parse error: {e}");
            let err = serde_json::json!({
                "error": {
                    "message": format!("Invalid request body: {e}"),
                    "type": "invalid_request_error",
                    "code": "bad_request"
                }
            });
            return (StatusCode::BAD_REQUEST, Json(err)).into_response();
        }
    };

    // ── Outpost: scan any tool result messages before sending to LLM ───
    // This happens for BOTH streaming and non-streaming paths.
    // Security constraint: tool results are scanned BEFORE reaching the LLM.
    let mut scanned_messages = scan_messages(&req.messages).await;

    // ── Inject AGENTS.md as system prompt if none provided ─────────────
    let workspace_dir = {
        let cfg = state.config.lock();
        cfg.workspace_dir.clone()
    };
    inject_agents_md_if_needed(&mut scanned_messages, &workspace_dir).await;

    // ── Build provider messages ─────────────────────────────────────────
    let provider_messages = to_provider_messages(&scanned_messages);

    let model = req
        .model
        .clone()
        .unwrap_or_else(|| state.model.clone());
    
    // Use config's default_temperature if request didn't specify (or used default)
    let temperature = {
        let cfg = state.config.lock();
        if (req.temperature - 0.7).abs() < f64::EPSILON {
            // Request used the hardcoded default, use config value instead
            cfg.default_temperature
        } else {
            req.temperature
        }
    };

    // ── Route: streaming vs non-streaming ──────────────────────────────
    if req.stream {
        handle_streaming(state, provider_messages, model, temperature).await
    } else {
        handle_non_streaming(state, provider_messages, model, temperature).await
    }
}

/// Non-streaming response path — runs the full agent tool-call loop.
async fn handle_non_streaming(
    state: AppState,
    provider_messages: Vec<crate::providers::ChatMessage>,
    model: String,
    temperature: f64,
) -> axum::response::Response {
    let (provider_name, max_tool_iterations, multimodal_config) = {
        let cfg = state.config.lock();
        let provider_name = cfg
            .default_provider
            .clone()
            .unwrap_or_else(|| "gateway".to_string());
        let max_tool_iterations = cfg.agent.max_tool_iterations;
        let multimodal_config = cfg.multimodal.clone();
        (provider_name, max_tool_iterations, multimodal_config)
    };

    let mut history = provider_messages;

    let result = crate::agent::loop_::run_tool_call_loop(
        state.provider.as_ref(),
        &mut history,
        state.tools_for_loop.as_ref(),
        &crate::observability::NoopObserver,
        &provider_name,
        &model,
        temperature,
        true,  // silent — no stdout progress in gateway context
        None,  // no interactive approval
        "gateway",
        &multimodal_config,
        max_tool_iterations,
        None,  // no cancellation token
        None,  // no delta streaming for non-streaming path
        None,  // no hooks
        &[],   // no excluded tools
        None,  // no clash policy
        "",    // no policy identity
        None,  // no pending_approvals
        None,  // no config_snapshot
        "",    // no sender_key_for_review
    )
    .await;

    match result {
        Ok(reply) => {
            let response = ChatCompletionResponse {
                id: format!("chatcmpl-{}", Uuid::new_v4()),
                object: "chat.completion".to_string(),
                created: unix_now(),
                model: model.clone(),
                choices: vec![Choice {
                    index: 0,
                    message: ChatCompletionMessage {
                        role: "assistant".to_string(),
                        content: Some(reply),
                        tool_call_id: None,
                        name: None,
                    },
                    finish_reason: "stop".to_string(),
                }],
            };
            Json(serde_json::to_value(response).unwrap_or_default()).into_response()
        }
        Err(e) => {
            let sanitized = crate::providers::sanitize_api_error(&e.to_string());
            tracing::warn!("OpenAI-compat tool loop error: {sanitized}");

            // On tool-loop exhaustion, return the last assistant text from history
            // rather than a bare error. This happens when the model burns through
            // max_tool_iterations without producing a final plain-text reply.
            let last_text = history.iter().rev().find_map(|m| {
                if m.role == "assistant" {
                    let text = m.content.trim();
                    if !text.is_empty() && !text.contains("<tool_call>") {
                        return Some(text.to_string());
                    }
                }
                None
            });

            if let Some(fallback) = last_text {
                tracing::info!("Returning last assistant text as fallback after tool loop error");
                let response = ChatCompletionResponse {
                    id: format!("chatcmpl-{}", Uuid::new_v4()),
                    object: "chat.completion".to_string(),
                    created: unix_now(),
                    model: model.clone(),
                    choices: vec![Choice {
                        index: 0,
                        message: ChatCompletionMessage {
                            role: "assistant".to_string(),
                            content: Some(fallback),
                            tool_call_id: None,
                            name: None,
                        },
                        finish_reason: "stop".to_string(),
                    }],
                };
                return Json(serde_json::to_value(response).unwrap_or_default()).into_response();
            }

            let err = serde_json::json!({
                "error": {
                    "message": "LLM request failed",
                    "type": "server_error",
                    "code": "llm_error"
                }
            });
            (StatusCode::INTERNAL_SERVER_ERROR, Json(err)).into_response()
        }
    }
}

/// Streaming response path — runs the full agent tool-call loop and emits
/// OpenAI-compatible SSE events.
///
/// The tool-call loop accepts an `on_delta: Option<Sender<String>>` channel.
/// We wire this to an SSE event stream so tokens arrive progressively.
/// The loop sends a `DRAFT_CLEAR_SENTINEL` before streaming the final answer
/// text — we filter that out and only forward actual content chunks.
///
/// Progress messages (🤔 Thinking..., 💬 Got N tool call(s)) are forwarded
/// as content chunks too, giving the client visibility into tool execution.
async fn handle_streaming(
    state: AppState,
    provider_messages: Vec<crate::providers::ChatMessage>,
    model: String,
    temperature: f64,
) -> axum::response::Response {
    let completion_id = format!("chatcmpl-{}", Uuid::new_v4());
    let created = unix_now();

    let (provider_name, max_tool_iterations, multimodal_config) = {
        let cfg = state.config.lock();
        let provider_name = cfg
            .default_provider
            .clone()
            .unwrap_or_else(|| "gateway".to_string());
        let max_tool_iterations = cfg.agent.max_tool_iterations;
        let multimodal_config = cfg.multimodal.clone();
        (provider_name, max_tool_iterations, multimodal_config)
    };

    // Channel: tool-call loop → SSE emitter
    // on_delta receives plain String chunks (progress notes + final text).
    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<String>(64);

    // SSE event channel: emitter → ReceiverStream
    let (sse_tx, sse_rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(64);
    let sse_tx_for_loop = sse_tx.clone();

    // Spawn the tool-call loop
    tokio::spawn(async move {
        let mut history = provider_messages;
        let _result = crate::agent::loop_::run_tool_call_loop(
            state.provider.as_ref(),
            &mut history,
            state.tools_for_loop.as_ref(),
            &crate::observability::NoopObserver,
            &provider_name,
            &model,
            temperature,
            true,  // silent — no stdout progress
            None,  // no interactive approval
            "gateway",
            &multimodal_config,
            max_tool_iterations,
            None,  // no cancellation token
            Some(delta_tx),
            None,  // no hooks
            &[],   // no excluded tools
            None,  // no clash policy
            "",    // no policy identity
            None,  // no pending_approvals
            None,  // no config_snapshot
            "",    // no sender_key_for_review
        )
        .await;
        // delta_tx dropped here — delta_rx will see channel closed
    });

    // Spawn SSE emitter: reads from delta_rx, writes to sse_tx
    tokio::spawn(async move {
        let id = completion_id.clone();
        let model_id = id.clone();

        // Send initial role chunk so client knows we're assistant
        let role_chunk = ChatCompletionChunk {
            id: id.clone(),
            object: "chat.completion.chunk".to_string(),
            created,
            model: model_id.clone(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: Some("assistant".to_string()),
                    content: None,
                },
                finish_reason: None,
            }],
        };
        if let Ok(json) = serde_json::to_string(&role_chunk) {
            let _ = sse_tx_for_loop.send(Ok(Event::default().data(json))).await;
        }

        // Relay all delta chunks, filtering sentinel and progress notes.
        // The tool loop sends progress annotations (🤔 Thinking..., ⏳ tool:, ✅/❌ result)
        // through on_delta alongside actual content. In channel mode these update
        // a "draft" message; in gateway mode we only want the final answer text.
        while let Some(chunk) = delta_rx.recv().await {
            // Filter out the draft-clear sentinel — it's a channel/UI artifact
            if chunk == crate::agent::loop_::DRAFT_CLEAR_SENTINEL {
                continue;
            }
            if chunk.is_empty() {
                continue;
            }
            // Filter progress notes — they start with tool-loop emoji prefixes.
            // These are UI annotations for draft messages, not final answer text.
            let trimmed = chunk.trim_start();
            if trimmed.starts_with("🤔")   // Thinking...
                || trimmed.starts_with("💬") // Got N tool call(s)
                || trimmed.starts_with("⏳") // tool running
                || trimmed.starts_with("✅") // tool success
                || trimmed.starts_with("❌") // tool failure
                || trimmed.starts_with("⚠️") // warning
                || trimmed.starts_with("🔄") // retry
            {
                continue;
            }

            let content_chunk = ChatCompletionChunk {
                id: id.clone(),
                object: "chat.completion.chunk".to_string(),
                created,
                model: model_id.clone(),
                choices: vec![ChunkChoice {
                    index: 0,
                    delta: ChunkDelta {
                        role: None,
                        content: Some(chunk),
                    },
                    finish_reason: None,
                }],
            };
            if let Ok(json) = serde_json::to_string(&content_chunk) {
                if sse_tx_for_loop
                    .send(Ok(Event::default().data(json)))
                    .await
                    .is_err()
                {
                    return; // Client disconnected
                }
            }
        }

        // Loop finished — emit stop chunk + [DONE]
        let stop_chunk = ChatCompletionChunk {
            id: id.clone(),
            object: "chat.completion.chunk".to_string(),
            created,
            model: model_id.clone(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: None,
                    content: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
        };
        if let Ok(json) = serde_json::to_string(&stop_chunk) {
            let _ = sse_tx_for_loop.send(Ok(Event::default().data(json))).await;
        }
        let _ = sse_tx_for_loop
            .send(Ok(Event::default().data("[DONE]")))
            .await;
    });

    let event_stream = ReceiverStream::new(sse_rx);
    Sse::new(event_stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// `GET /v1/models` — returns a static model list stub.
pub async fn handle_list_models(State(state): State<AppState>) -> impl IntoResponse {
    let model = state.model.clone();
    let now = unix_now();

    let response = ModelsResponse {
        object: "list".to_string(),
        data: vec![ModelEntry {
            id: model,
            object: "model".to_string(),
            created: now,
            owned_by: "nonzeroclaw".to_string(),
        }],
    };

    Json(serde_json::to_value(response).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_scan_context_only_for_tool_role() {
        let user_msg = ChatCompletionMessage {
            role: "user".to_string(),
            content: Some("hello".to_string()),
            tool_call_id: None,
            name: None,
        };
        assert!(detect_scan_context(&user_msg).is_none());

        let system_msg = ChatCompletionMessage {
            role: "system".to_string(),
            content: Some("you are helpful".to_string()),
            tool_call_id: None,
            name: None,
        };
        assert!(detect_scan_context(&system_msg).is_none());
    }

    #[test]
    fn detect_scan_context_html_is_web_fetch() {
        let msg = ChatCompletionMessage {
            role: "tool".to_string(),
            content: Some("<!DOCTYPE html><html>...</html>".to_string()),
            tool_call_id: Some("call_1".to_string()),
            name: None,
        };
        assert!(matches!(detect_scan_context(&msg), Some(ScanContext::WebFetch)));
    }

    #[test]
    fn detect_scan_context_json_is_api() {
        let msg = ChatCompletionMessage {
            role: "tool".to_string(),
            content: Some(r#"{"key":"value"}"#.to_string()),
            tool_call_id: None,
            name: None,
        };
        assert!(matches!(detect_scan_context(&msg), Some(ScanContext::Api)));
    }

    #[test]
    fn detect_scan_context_plain_text_is_exec() {
        let msg = ChatCompletionMessage {
            role: "tool".to_string(),
            content: Some("command output here".to_string()),
            tool_call_id: None,
            name: None,
        };
        assert!(matches!(detect_scan_context(&msg), Some(ScanContext::Exec)));
    }

    #[tokio::test]
    async fn scan_tool_output_clean_passthrough() {
        let output = "simple clean output";
        let result = scan_tool_output(output, "test_tool", ScanContext::Exec).await;
        assert_eq!(result, output);
    }
}
