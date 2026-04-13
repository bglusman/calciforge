//! Anthropic provider implementation
//!
//! Anthropic uses a different API format than OpenAI.

#![allow(dead_code)]

use super::{Provider, ProviderConfig, ProviderType};
use crate::proxy::backend::BackendError;
use crate::proxy::openai::{
    ChatCompletionResponse, ChatMessage, MessageContent, ToolChoice, ToolDefinition,
};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use std::collections::HashMap;

pub struct AnthropicProvider {
    config: ProviderConfig,
    http_client: Client,
}

impl AnthropicProvider {
    fn base_url(&self) -> String {
        "https://api.anthropic.com/v1".to_string()
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn new(config: &ProviderConfig) -> Self {
        Self {
            config: config.clone(),
            http_client: Client::new(),
        }
    }

    fn key(&self) -> String {
        self.config.id.clone()
    }

    fn r#type(&self) -> ProviderType {
        ProviderType::Anthropic
    }

    async fn chat_completions(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        stream: bool,
        tools: Option<Vec<ToolDefinition>>,
        tool_choice: Option<ToolChoice>,
    ) -> Result<ChatCompletionResponse, BackendError> {
        // Convert messages to Anthropic format
        let anthropic_messages: Vec<HashMap<String, serde_json::Value>> = messages
            .into_iter()
            .map(|msg| {
                let mut message = HashMap::new();
                message.insert("role".to_string(), serde_json::Value::String(msg.role));

                match msg.content {
                    Some(MessageContent::Text(text)) => {
                        message.insert(
                            "content".to_string(),
                            json!([{
                                "type": "text",
                                "text": text
                            }]),
                        );
                    }
                    Some(MessageContent::Parts(parts)) => {
                        // Convert parts to Anthropic format
                        let content: Vec<serde_json::Value> = parts
                            .into_iter()
                            .map(|part| {
                                if part.r#type == "text" {
                                    json!({
                                        "type": "text",
                                        "text": part.text.unwrap_or_default()
                                    })
                                } else {
                                    // For non-text parts, we'd need to handle them differently
                                    json!({
                                        "type": "text",
                                        "text": "[non-text content]"
                                    })
                                }
                            })
                            .collect();
                        message.insert("content".to_string(), serde_json::Value::Array(content));
                    }
                    None => {
                        message.insert(
                            "content".to_string(),
                            json!([{
                                "type": "text",
                                "text": ""
                            }]),
                        );
                    }
                }

                message
            })
            .collect();

        // Build request payload for Anthropic
        let mut payload = json!({
            "model": model,
            "messages": anthropic_messages,
            "max_tokens": 4096, // Default value
        });

        // Add tools if provided (Anthropic calls them "tools")
        if let Some(tools) = tools {
            let tool_defs: Vec<serde_json::Value> = tools
                .into_iter()
                .map(|tool| {
                    json!({
                        "name": tool.function.name,
                        "description": tool.function.description,
                        "input_schema": tool.function.parameters,
                    })
                })
                .collect();

            payload["tools"] = serde_json::Value::Array(tool_defs);

            // Add tool_choice if provided
            if let Some(tool_choice) = tool_choice {
                let tool_choice_value = match tool_choice {
                    ToolChoice::Mode(mode) => serde_json::Value::String(mode),
                    ToolChoice::Specific { r#type, function } => {
                        // For function-specific tool choice
                        json!({
                            "type": r#type,
                            "function": {
                                "name": function.name
                            }
                        })
                    }
                };
                payload["tool_choice"] = tool_choice_value;
            }
        }

        // Make the request to Anthropic API
        let response = self
            .http_client
            .post(format!("{}/messages", self.base_url()))
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| {
                BackendError::ExecutionFailed(format!("Anthropic API request error: {}", e))
            })?;

        let status = response.status();

        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(BackendError::ExecutionFailed(format!(
                "Anthropic API error ({}): {}",
                status, error_text
            )));
        }

        if stream {
            // For streaming responses
            Err(BackendError::NotAvailable(
                "Streaming not yet implemented for Anthropic provider".to_string(),
            ))
        } else {
            // Parse the response
            let response_json: serde_json::Value = response.json().await.map_err(|e| {
                BackendError::InvalidResponse(format!("Failed to parse response: {}", e))
            })?;

            // Convert Anthropic response to OpenAI format
            let id = response_json["id"].as_str().unwrap_or("").to_string();

            // Extract text content from Anthropic response
            let content = response_json["content"]
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .filter_map(|item| {
                    if item["type"].as_str() == Some("text") {
                        item["text"].as_str().map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect::<Vec<String>>()
                .join(" ");

            // Handle tool calls from Anthropic
            let tool_calls = response_json["content"]
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .filter(|item| item["type"].as_str() == Some("tool_use"))
                .map(|item| crate::proxy::openai::ToolCall {
                    id: item["id"].as_str().unwrap_or("").to_string(),
                    r#type: "function".to_string(),
                    function: crate::proxy::openai::FunctionCall {
                        name: item["name"].as_str().unwrap_or("").to_string(),
                        arguments: serde_json::to_string(&item["input"])
                            .unwrap_or_else(|_| "{}".to_string()),
                    },
                })
                .collect::<Vec<_>>();

            let choices = vec![crate::proxy::openai::Choice {
                index: 0,
                message: crate::proxy::openai::ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(MessageContent::Text(content)),
                    name: None,
                    tool_calls: if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    },
                    tool_call_id: None,
                },
                finish_reason: response_json["stop_reason"].as_str().map(|s| s.to_string()),
                logprobs: None,
            }];

            // Extract usage from Anthropic response
            let usage = if let Some(input_tokens) = response_json["usage"]["input_tokens"].as_u64()
            {
                crate::proxy::openai::Usage {
                    prompt_tokens: input_tokens as u32,
                    completion_tokens: response_json["usage"]["output_tokens"]
                        .as_u64()
                        .unwrap_or(0) as u32,
                    total_tokens: (input_tokens
                        + response_json["usage"]["output_tokens"]
                            .as_u64()
                            .unwrap_or(0)) as u32,
                }
            } else {
                crate::proxy::openai::Usage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                }
            };

            Ok(ChatCompletionResponse {
                id,
                object: "chat.completion".to_string(),
                created: chrono::Utc::now().timestamp() as u64,
                model: model,
                choices,
                usage,
                system_fingerprint: None,
            })
        }
    }
}
