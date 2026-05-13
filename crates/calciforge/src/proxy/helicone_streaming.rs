use std::collections::HashMap;

use crate::proxy::{
    backend::BackendError,
    openai::{
        ChatCompletionResponse, ChatMessage, Choice, FunctionCall, MessageContent, ToolCall, Usage,
    },
};

#[derive(Debug, Default)]
struct ToolCallAccumulator {
    id: Option<String>,
    r#type: Option<String>,
    name: Option<String>,
    arguments: String,
}

pub(super) fn parse_streaming_chat_completion(
    body: &str,
    requested_model: &str,
) -> Result<ChatCompletionResponse, BackendError> {
    let mut id: Option<String> = None;
    let mut created: Option<u64> = None;
    let mut model: Option<String> = None;
    let mut role: Option<String> = None;
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut reasoning_content = String::new();
    let mut finish_reason: Option<String> = None;
    let mut usage = Usage {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
    };
    let mut tool_calls: HashMap<usize, ToolCallAccumulator> = HashMap::new();
    let mut saw_chunk = false;

    for event in body.split("\n\n") {
        let mut data = String::new();
        for line in event.lines() {
            let Some(rest) = line.strip_prefix("data:") else {
                continue;
            };
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.trim_start());
        }
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }

        saw_chunk = true;
        let value: serde_json::Value = serde_json::from_str(data).map_err(|e| {
            BackendError::InvalidResponse(format!(
                "Failed to parse Helicone streaming chunk for model '{}': {}",
                requested_model, e
            ))
        })?;

        if id.is_none() {
            id = value
                .get("id")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned);
        }
        if created.is_none() {
            created = value.get("created").and_then(|v| v.as_u64());
        }
        if model.is_none() {
            model = value
                .get("model")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned);
        }
        if let Some(chunk_usage) = value.get("usage") {
            usage.prompt_tokens = chunk_usage
                .get("prompt_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(usage.prompt_tokens as u64) as u32;
            usage.completion_tokens = chunk_usage
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(usage.completion_tokens as u64)
                as u32;
            usage.total_tokens = chunk_usage
                .get("total_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(usage.total_tokens as u64) as u32;
        }

        let choices = value
            .get("choices")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                BackendError::InvalidResponse(format!(
                    "Helicone streaming chunk for model '{}' did not include choices",
                    requested_model
                ))
            })?;
        for choice in choices {
            if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                finish_reason = Some(reason.to_string());
            }
            let Some(delta) = choice.get("delta").and_then(|v| v.as_object()) else {
                continue;
            };
            if role.is_none() {
                role = delta
                    .get("role")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned);
            }
            if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                content.push_str(text);
            }
            if let Some(text) = delta.get("reasoning").and_then(|v| v.as_str()) {
                reasoning.push_str(text);
            }
            if let Some(text) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
                reasoning_content.push_str(text);
            }
            if let Some(calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for (fallback_index, call) in calls.iter().enumerate() {
                    let index = call
                        .get("index")
                        .and_then(|v| v.as_u64())
                        .map(|i| i as usize)
                        .unwrap_or(fallback_index);
                    let entry = tool_calls.entry(index).or_default();
                    if let Some(value) = call.get("id").and_then(|v| v.as_str()) {
                        entry.id = Some(value.to_string());
                    }
                    if let Some(value) = call.get("type").and_then(|v| v.as_str()) {
                        entry.r#type = Some(value.to_string());
                    }
                    if let Some(function) = call.get("function") {
                        if let Some(value) = function.get("name").and_then(|v| v.as_str()) {
                            entry.name = Some(value.to_string());
                        }
                        if let Some(value) = function.get("arguments").and_then(|v| v.as_str()) {
                            entry.arguments.push_str(value);
                        }
                    }
                }
            }
        }
    }

    if !saw_chunk {
        return Err(BackendError::InvalidResponse(format!(
            "Helicone streaming response for model '{}' did not include any chunks",
            requested_model
        )));
    }

    let mut tool_call_entries: Vec<(usize, ToolCall)> = Vec::new();
    for (index, call) in tool_calls {
        let Some(id) = call.id else {
            continue;
        };
        let Some(name) = call.name else {
            continue;
        };
        tool_call_entries.push((
            index,
            ToolCall {
                id,
                r#type: call.r#type.unwrap_or_else(|| "function".to_string()),
                function: FunctionCall {
                    name,
                    arguments: call.arguments,
                },
            },
        ));
    }
    tool_call_entries.sort_by_key(|(index, _)| *index);
    let parsed_tool_calls: Vec<ToolCall> = tool_call_entries
        .into_iter()
        .map(|(_, call)| call)
        .collect();

    let content = if content.is_empty() {
        None
    } else {
        Some(MessageContent::Text(content))
    };

    Ok(ChatCompletionResponse {
        id: id.unwrap_or_else(|| "chatcmpl-helicone-stream".to_string()),
        object: "chat.completion".to_string(),
        created: created.unwrap_or(0),
        model: model.unwrap_or_else(|| requested_model.to_string()),
        choices: vec![Choice {
            index: 0,
            message: ChatMessage {
                role: role.unwrap_or_else(|| "assistant".to_string()),
                content,
                name: None,
                tool_calls: if parsed_tool_calls.is_empty() {
                    None
                } else {
                    Some(parsed_tool_calls)
                },
                tool_call_id: None,
                reasoning: if reasoning.is_empty() {
                    None
                } else {
                    Some(reasoning)
                },
                reasoning_content: if reasoning_content.is_empty() {
                    None
                } else {
                    Some(reasoning_content)
                },
            },
            finish_reason,
            logprobs: None,
        }],
        usage,
        system_fingerprint: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_streaming_content_chunks() {
        let body = concat!(
            "data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"ollama/qwen3.6:27b\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"ollama/qwen3.6:27b\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"po\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-stream\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"ollama/qwen3.6:27b\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ng\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
            "data: [DONE]\n\n",
        );

        let result = parse_streaming_chat_completion(body, "ollama/qwen3.6:27b").unwrap();

        assert_eq!(result.model, "ollama/qwen3.6:27b");
        assert_eq!(
            result.choices[0]
                .message
                .content
                .as_ref()
                .and_then(MessageContent::to_text)
                .as_deref(),
            Some("pong")
        );
        assert_eq!(result.choices[0].finish_reason.as_deref(), Some("stop"));
        assert_eq!(result.usage.total_tokens, 2);
    }

    #[test]
    fn parses_streamed_tool_calls() {
        let body = concat!(
            "data: {\"id\":\"chatcmpl-tools\",\"object\":\"chat.completion.chunk\",\"created\":2,\"model\":\"ollama/qwen3.6:27b\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"fetch_url\",\"arguments\":\"{\\\"url\\\":\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-tools\",\"object\":\"chat.completion.chunk\",\"created\":2,\"model\":\"ollama/qwen3.6:27b\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"https://example.test\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );

        let result = parse_streaming_chat_completion(body, "ollama/qwen3.6:27b").unwrap();
        let tool_calls = result.choices[0]
            .message
            .tool_calls
            .as_ref()
            .expect("streamed tool calls should be preserved");

        assert_eq!(
            result.choices[0].finish_reason.as_deref(),
            Some("tool_calls")
        );
        assert_eq!(tool_calls[0].id, "call_1");
        assert_eq!(tool_calls[0].function.name, "fetch_url");
        assert_eq!(
            tool_calls[0].function.arguments,
            "{\"url\":\"https://example.test\"}"
        );
    }
}
