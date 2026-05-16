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

#[derive(Debug, Default)]
struct ChoiceAccumulator {
    role: Option<String>,
    content: String,
    reasoning: String,
    reasoning_content: String,
    finish_reason: Option<String>,
    tool_calls: HashMap<u32, ToolCallAccumulator>,
}

pub(super) fn parse_streaming_chat_completion(
    body: &str,
    requested_model: &str,
) -> Result<ChatCompletionResponse, BackendError> {
    let mut id: Option<String> = None;
    let mut created: Option<u64> = None;
    let mut model: Option<String> = None;
    let mut usage = Usage {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
    };
    let mut choices_by_index: HashMap<u32, ChoiceAccumulator> = HashMap::new();
    let mut saw_chunk = false;
    let normalized_body = body.replace("\r\n", "\n").replace('\r', "\n");

    for event in normalized_body.split("\n\n") {
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
                "Failed to parse OpenAI-compatible streaming chunk for model '{}': {}",
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
            usage.prompt_tokens =
                parse_usage_count(chunk_usage, "prompt_tokens", usage.prompt_tokens)?;
            usage.completion_tokens =
                parse_usage_count(chunk_usage, "completion_tokens", usage.completion_tokens)?;
            usage.total_tokens =
                parse_usage_count(chunk_usage, "total_tokens", usage.total_tokens)?;
        }

        let choices = value
            .get("choices")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                BackendError::InvalidResponse(format!(
                    "OpenAI-compatible streaming chunk for model '{}' did not include choices",
                    requested_model
                ))
            })?;
        for choice in choices {
            let index = choice
                .get("index")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| {
                    BackendError::InvalidResponse(format!(
                        "OpenAI-compatible streaming choice for model '{}' did not include a numeric index",
                        requested_model
                    ))
                })?;
            let index = u32::try_from(index).map_err(|_| {
                BackendError::InvalidResponse(format!(
                    "OpenAI-compatible streaming choice index for model '{}' exceeded u32",
                    requested_model
                ))
            })?;
            let accumulator = choices_by_index.entry(index).or_default();

            if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                accumulator.finish_reason = Some(reason.to_string());
            }
            let Some(delta) = choice.get("delta").and_then(|v| v.as_object()) else {
                continue;
            };
            if accumulator.role.is_none() {
                accumulator.role = delta
                    .get("role")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned);
            }
            if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                accumulator.content.push_str(text);
            }
            if let Some(text) = delta.get("reasoning").and_then(|v| v.as_str()) {
                accumulator.reasoning.push_str(text);
            }
            if let Some(text) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
                accumulator.reasoning_content.push_str(text);
            }
            if let Some(calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for (fallback_index, call) in calls.iter().enumerate() {
                    let index = call
                        .get("index")
                        .and_then(|v| v.as_u64())
                        .map(u32::try_from)
                        .transpose()
                        .map_err(|_| {
                            BackendError::InvalidResponse(format!(
                                "OpenAI-compatible streaming tool call index for model '{}' exceeded u32",
                                requested_model
                            ))
                        })?
                        .unwrap_or(u32::try_from(fallback_index).map_err(|_| {
                            BackendError::InvalidResponse(format!(
                                "OpenAI-compatible streaming tool call index for model '{}' exceeded u32",
                                requested_model
                            ))
                        })?);
                    let entry = accumulator.tool_calls.entry(index).or_default();
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
            "OpenAI-compatible streaming response for model '{}' did not include any chunks",
            requested_model
        )));
    }

    let Some(id) = id else {
        return Err(BackendError::InvalidResponse(format!(
            "OpenAI-compatible streaming response for model '{}' did not include an id",
            requested_model
        )));
    };
    let Some(created) = created else {
        return Err(BackendError::InvalidResponse(format!(
            "OpenAI-compatible streaming response for model '{}' did not include created",
            requested_model
        )));
    };

    let mut choice_entries: Vec<(u32, Choice)> = Vec::new();
    for (index, accumulator) in choices_by_index {
        let mut tool_call_entries: Vec<(u32, ToolCall)> = Vec::new();
        for (tool_index, call) in accumulator.tool_calls {
            let id = call.id.ok_or_else(|| {
                BackendError::InvalidResponse(format!(
                    "OpenAI-compatible streaming tool call for model '{}' did not include an id",
                    requested_model
                ))
            })?;
            let name = call.name.ok_or_else(|| {
                BackendError::InvalidResponse(format!(
                    "OpenAI-compatible streaming tool call for model '{}' did not include a function name",
                    requested_model
                ))
            })?;
            tool_call_entries.push((
                tool_index,
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

        let content = if accumulator.content.is_empty() {
            None
        } else {
            Some(MessageContent::Text(accumulator.content))
        };

        choice_entries.push((
            index,
            Choice {
                index,
                message: ChatMessage {
                    role: accumulator.role.unwrap_or_else(|| "assistant".to_string()),
                    content,
                    name: None,
                    tool_calls: if parsed_tool_calls.is_empty() {
                        None
                    } else {
                        Some(parsed_tool_calls)
                    },
                    tool_call_id: None,
                    reasoning: if accumulator.reasoning.is_empty() {
                        None
                    } else {
                        Some(accumulator.reasoning)
                    },
                    reasoning_content: if accumulator.reasoning_content.is_empty() {
                        None
                    } else {
                        Some(accumulator.reasoning_content)
                    },
                },
                finish_reason: accumulator.finish_reason,
                logprobs: None,
            },
        ));
    }

    if choice_entries.is_empty() {
        return Err(BackendError::InvalidResponse(format!(
            "OpenAI-compatible streaming response for model '{}' did not include any choices",
            requested_model
        )));
    }
    choice_entries.sort_by_key(|(index, _)| *index);
    let choices = choice_entries
        .into_iter()
        .map(|(_, choice)| choice)
        .collect();

    Ok(ChatCompletionResponse {
        id,
        object: "chat.completion".to_string(),
        created,
        model: model.unwrap_or_else(|| requested_model.to_string()),
        choices,
        usage,
        system_fingerprint: None,
        extra_body: serde_json::Map::new(),
    })
}

fn parse_usage_count(
    usage: &serde_json::Value,
    field: &str,
    previous: u32,
) -> Result<u32, BackendError> {
    let Some(value) = usage.get(field).and_then(|v| v.as_u64()) else {
        return Ok(previous);
    };
    u32::try_from(value).map_err(|_| {
        BackendError::InvalidResponse(format!(
            "OpenAI-compatible streaming usage field '{}' exceeded u32",
            field
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    fn sse_event(value: serde_json::Value, delimiter: &str) -> String {
        format!("data: {value}{delimiter}")
    }

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

    #[test]
    fn parses_crlf_delimited_streaming_chunks() {
        let body = concat!(
            "data: {\"id\":\"chatcmpl-crlf\",\"object\":\"chat.completion.chunk\",\"created\":3,\"model\":\"ollama/qwen3.6:27b\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"po\"},\"finish_reason\":null}]}\r\n\r\n",
            "data: {\"id\":\"chatcmpl-crlf\",\"object\":\"chat.completion.chunk\",\"created\":3,\"model\":\"ollama/qwen3.6:27b\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ng\"},\"finish_reason\":\"stop\"}]}\r\n\r\n",
            "data: [DONE]\r\n\r\n",
        );

        let result = parse_streaming_chat_completion(body, "ollama/qwen3.6:27b").unwrap();

        assert_eq!(
            result.choices[0]
                .message
                .content
                .as_ref()
                .and_then(MessageContent::to_text)
                .as_deref(),
            Some("pong")
        );
    }

    #[test]
    fn preserves_multiple_streamed_choices_by_index() {
        let body = concat!(
            "data: {\"id\":\"chatcmpl-multi\",\"object\":\"chat.completion.chunk\",\"created\":4,\"model\":\"ollama/qwen3.6:27b\",\"choices\":[{\"index\":1,\"delta\":{\"role\":\"assistant\",\"content\":\"b\"},\"finish_reason\":null},{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"a\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-multi\",\"object\":\"chat.completion.chunk\",\"created\":4,\"model\":\"ollama/qwen3.6:27b\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"0\"},\"finish_reason\":\"stop\"},{\"index\":1,\"delta\":{\"content\":\"1\"},\"finish_reason\":\"length\"}]}\n\n",
            "data: [DONE]\n\n",
        );

        let result = parse_streaming_chat_completion(body, "ollama/qwen3.6:27b").unwrap();

        assert_eq!(result.choices.len(), 2);
        assert_eq!(result.choices[0].index, 0);
        assert_eq!(
            result.choices[0]
                .message
                .content
                .as_ref()
                .and_then(MessageContent::to_text)
                .as_deref(),
            Some("a0")
        );
        assert_eq!(result.choices[1].index, 1);
        assert_eq!(
            result.choices[1]
                .message
                .content
                .as_ref()
                .and_then(MessageContent::to_text)
                .as_deref(),
            Some("b1")
        );
        assert_eq!(result.choices[1].finish_reason.as_deref(), Some("length"));
    }

    #[test]
    fn rejects_missing_required_stream_metadata() {
        let body = concat!(
            "data: {\"object\":\"chat.completion.chunk\",\"model\":\"ollama/qwen3.6:27b\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"pong\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );

        let error = parse_streaming_chat_completion(body, "ollama/qwen3.6:27b")
            .expect_err("missing id/created should not be papered over");

        assert!(
            error.to_string().contains("did not include an id"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_incomplete_streamed_tool_calls() {
        let body = concat!(
            "data: {\"id\":\"chatcmpl-tools\",\"object\":\"chat.completion.chunk\",\"created\":6,\"model\":\"ollama/qwen3.6:27b\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"type\":\"function\",\"function\":{\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );

        let error = parse_streaming_chat_completion(body, "ollama/qwen3.6:27b")
            .expect_err("missing tool call id/name should not become a successful response");

        assert!(
            error.to_string().contains("did not include an id"),
            "unexpected error: {error}"
        );
    }

    proptest! {
        #[test]
        fn arbitrary_stream_bodies_never_panic(body in ".*") {
            let _ = parse_streaming_chat_completion(&body, "generated/model");
        }

        #[test]
        fn valid_generated_streams_preserve_content(
            fragments in prop::collection::vec("[a-zA-Z0-9 _.-]{1,12}", 1..8),
            delimiter in prop_oneof![Just("\n\n"), Just("\r\n\r\n")],
        ) {
            let expected = fragments.concat();
            let mut body = String::new();
            body.push_str(&sse_event(json!({
                "id": "chatcmpl-generated",
                "object": "chat.completion.chunk",
                "created": 5,
                "model": "generated/model",
                "choices": [{
                    "index": 0,
                    "delta": {"role": "assistant"},
                    "finish_reason": null
                }]
            }), delimiter));
            for fragment in &fragments {
                body.push_str(&sse_event(json!({
                    "id": "chatcmpl-generated",
                    "object": "chat.completion.chunk",
                    "created": 5,
                    "model": "generated/model",
                    "choices": [{
                        "index": 0,
                        "delta": {"content": fragment},
                        "finish_reason": null
                    }]
                }), delimiter));
            }
            body.push_str(&sse_event(json!({
                "id": "chatcmpl-generated",
                "object": "chat.completion.chunk",
                "created": 5,
                "model": "generated/model",
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1,
                    "total_tokens": 2
                }
            }), delimiter));
            body.push_str(&format!("data: [DONE]{delimiter}"));

            let result = parse_streaming_chat_completion(&body, "generated/model").unwrap();
            let content = result.choices[0]
                .message
                .content
                .as_ref()
                .and_then(MessageContent::to_text);

            prop_assert_eq!(content.as_deref(), Some(expected.as_str()));
            prop_assert_eq!(result.choices[0].finish_reason.as_deref(), Some("stop"));
            prop_assert_eq!(result.usage.total_tokens, 2);
        }
    }
}
