//! OpenAI-compatible API types
//!
//! Request/response structures matching the OpenAI Chat Completions API
//! for maximum compatibility with existing agents and tools.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// OpenAI-style chat completion request
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    /// Model ID or alloy alias (e.g., "gpt-4", "alloy/free-tier")
    pub model: String,

    /// Messages for the conversation
    pub messages: Vec<ChatMessage>,

    /// Maximum tokens to generate (optional)
    #[serde(default)]
    pub max_tokens: Option<u32>,

    /// Temperature for sampling (optional, default 1.0)
    #[serde(default)]
    pub temperature: Option<f32>,

    /// Top-p sampling (optional)
    #[serde(default)]
    pub top_p: Option<f32>,

    /// Number of completions to generate (optional, default 1)
    #[serde(default)]
    pub n: Option<u32>,

    /// Whether to stream responses (optional, default false)
    #[serde(default)]
    pub stream: Option<bool>,

    /// Stop sequences (optional)
    #[serde(default)]
    pub stop: Option<Vec<String>>,

    /// Presence penalty (optional)
    #[serde(default)]
    pub presence_penalty: Option<f32>,

    /// Frequency penalty (optional)
    #[serde(default)]
    pub frequency_penalty: Option<f32>,

    /// Logit bias (optional)
    #[serde(default)]
    pub logit_bias: Option<HashMap<String, f32>>,

    /// User identifier for tracking (optional)
    #[serde(default)]
    pub user: Option<String>,

    /// Response format (optional, for JSON mode)
    #[serde(default)]
    pub response_format: Option<ResponseFormat>,

    /// Tool definitions for function calling (optional)
    #[serde(default)]
    pub tools: Option<Vec<ToolDefinition>>,

    /// Tool choice (optional)
    #[serde(default)]
    pub tool_choice: Option<ToolChoice>,

    /// Provider-specific OpenAI-compatible request extensions.
    ///
    /// Calciforge must preserve unknown fields such as Kimi's `thinking`,
    /// OpenAI reasoning knobs, or gateway-specific options when routing through
    /// the model gateway. Known fields remain first-class above; everything
    /// else round-trips here.
    #[serde(default, flatten)]
    pub extra_body: serde_json::Map<String, serde_json::Value>,
}

/// Content of a message - can be a simple string or an array of content parts
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    /// Simple text content
    Text(String),
    /// Array of content parts (text, images, etc.)
    Parts(Vec<ContentPart>),
}

/// A single content part (for multi-modal messages)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentPart {
    /// Type of content: "text" or "image_url"
    pub r#type: String,
    /// Text content (if type is "text")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Image URL (if type is "image_url")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<ImageUrl>,
}

/// Image URL specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrl {
    /// URL of the image (can be data URL)
    pub url: String,
    /// Optional detail level: "low", "high", or "auto"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl MessageContent {
    /// Convert content to a plain string (extracting text from parts if needed)
    pub fn to_text(&self) -> Option<String> {
        match self {
            MessageContent::Text(s) => Some(s.clone()),
            MessageContent::Parts(parts) => {
                // Concatenate all text parts
                let text: String = parts
                    .iter()
                    .filter(|p| p.r#type == "text")
                    .filter_map(|p| p.text.clone())
                    .collect();
                if text.is_empty() { None } else { Some(text) }
            }
        }
    }
}

/// A message in the conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Role: system, user, assistant, or tool
    pub role: String,

    /// Message content (can be null for tool calls, or a string/array)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,

    /// Name identifier (optional, for multi-user scenarios)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Tool calls made by the assistant (optional)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,

    /// Tool call ID this message is responding to (optional)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,

    /// Chain-of-thought reasoning content (Qwen3 thinking mode, DeepSeek-R1, etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,

    /// Alias used by some providers (e.g. DeepSeek) for reasoning content
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

/// Tool/function definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Type of tool (typically "function")
    pub r#type: String,

    /// Function definition
    pub function: FunctionDefinition,
}

/// Function definition for tools
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDefinition {
    /// Function name
    pub name: String,

    /// Function description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// JSON schema for function parameters
    pub parameters: serde_json::Value,
}

/// Tool choice configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    /// "none", "auto", or "required"
    Mode(String),
    /// Specific tool to use
    Specific {
        r#type: String,
        function: ToolChoiceFunction,
    },
}

/// Specific tool choice
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolChoiceFunction {
    pub name: String,
}

/// A tool call from the assistant
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Tool call ID
    pub id: String,

    /// Type (typically "function")
    pub r#type: String,

    /// Function call details
    pub function: FunctionCall,
}

/// Function call details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    /// Function name
    pub name: String,

    /// Arguments as JSON string
    pub arguments: String,
}

/// Response format specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseFormat {
    /// Type: "text" or "json_object"
    pub r#type: String,
}

/// OpenAI-style chat completion response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    /// Response ID
    pub id: String,

    /// Object type ("chat.completion")
    pub object: String,

    /// Creation timestamp (Unix epoch)
    pub created: u64,

    /// Model that generated the response
    pub model: String,

    /// Choices/completions
    pub choices: Vec<Choice>,

    /// Token usage statistics
    pub usage: Usage,

    /// System fingerprint (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_fingerprint: Option<String>,

    /// Provider/gateway-specific OpenAI-compatible response extensions.
    ///
    /// Wardwright, for example, returns `wardwright.receipt_id` so operators
    /// can inspect why a synthetic route selected a concrete provider. Preserve
    /// unknown response fields instead of silently dropping trace handles.
    #[serde(default, flatten)]
    pub extra_body: serde_json::Map<String, serde_json::Value>,
}

impl ChatCompletionResponse {
    pub fn wardwright_receipt_id(&self) -> Option<&str> {
        self.extra_body
            .get("wardwright")
            .and_then(|value| value.get("receipt_id"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                self.extra_body
                    .get("receipt_id")
                    .and_then(serde_json::Value::as_str)
            })
    }
}

/// A completion choice
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    /// Choice index
    pub index: u32,

    /// The message
    pub message: ChatMessage,

    /// Finish reason ("stop", "length", "tool_calls", "content_filter")
    pub finish_reason: Option<String>,

    /// Logprobs (optional, not implemented)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<serde_json::Value>,
}

/// Token usage statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    /// Prompt tokens
    pub prompt_tokens: u32,

    /// Completion tokens
    pub completion_tokens: u32,

    /// Total tokens
    pub total_tokens: u32,
}

/// Model listing response
#[derive(Debug, Clone, Serialize)]
pub struct ModelListResponse {
    pub object: String,
    pub data: Vec<ModelInfo>,
}

/// Model information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub owned_by: String,
}

/// Streaming chunk for SSE responses
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub system_fingerprint: Option<String>,
    pub choices: Vec<ChunkChoice>,
}

/// Choice within a streaming chunk
#[derive(Debug, Clone, Serialize)]
pub struct ChunkChoice {
    pub index: u32,
    pub delta: DeltaMessage,
    pub finish_reason: Option<String>,
    pub logprobs: Option<serde_json::Value>,
}

/// Delta message for streaming
#[derive(Debug, Clone, Default, Serialize)]
pub struct DeltaMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// API error response
#[derive(Debug, Clone, Serialize)]
pub struct ApiError {
    pub error: ErrorDetail,
}

/// Error detail
#[derive(Debug, Clone, Serialize)]
pub struct ErrorDetail {
    pub message: String,
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl ChatCompletionRequest {
    /// Get the effective model name, stripping alloy prefix if present
    pub fn effective_model(&self) -> &str {
        &self.model
    }

    /// Check if this request should use streaming
    pub fn should_stream(&self) -> bool {
        self.stream.unwrap_or(false)
    }
}

pub(crate) fn is_reserved_chat_completion_field(key: &str) -> bool {
    matches!(
        key,
        "model"
            | "messages"
            | "max_tokens"
            | "temperature"
            | "top_p"
            | "n"
            | "stream"
            | "stop"
            | "presence_penalty"
            | "frequency_penalty"
            | "logit_bias"
            | "user"
            | "response_format"
            | "tools"
            | "tool_choice"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_completion_request_preserves_provider_specific_fields() {
        let req: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "kimi-for-coding",
            "messages": [{"role": "user", "content": "hello"}],
            "thinking": {"type": "enabled"},
            "reasoning_effort": "high"
        }))
        .unwrap();

        assert_eq!(
            req.extra_body.get("thinking"),
            Some(&serde_json::json!({"type": "enabled"}))
        );
        assert_eq!(
            req.extra_body.get("reasoning_effort"),
            Some(&serde_json::json!("high"))
        );

        let serialized = serde_json::to_value(&req).unwrap();
        assert_eq!(
            serialized.get("thinking"),
            Some(&serde_json::json!({"type": "enabled"}))
        );
        assert_eq!(
            serialized.get("reasoning_effort"),
            Some(&serde_json::json!("high"))
        );
    }

    #[test]
    fn reserved_chat_completion_fields_are_not_provider_extensions() {
        assert!(is_reserved_chat_completion_field("model"));
        assert!(is_reserved_chat_completion_field("messages"));
        assert!(is_reserved_chat_completion_field("tool_choice"));
        assert!(!is_reserved_chat_completion_field("thinking"));
        assert!(!is_reserved_chat_completion_field("reasoning_effort"));
    }
}
