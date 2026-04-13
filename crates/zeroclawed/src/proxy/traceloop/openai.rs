//! OpenAI provider implementation
//!
//! Supports both OpenAI and OpenAI-compatible APIs (like Kimi).

#![allow(dead_code)]

use super::{Provider, ProviderConfig, ProviderType};
use crate::proxy::openai::{
    ChatCompletionResponse, ChatMessage, MessageContent, ToolDefinition, ToolChoice,
};
use crate::proxy::backend::BackendError;

/// Helper function to convert ToolChoice to a string representation
fn tool_choice_to_string(tool_choice: &ToolChoice) -> String {
    match tool_choice {
        ToolChoice::Mode(mode) => mode.clone(),
        ToolChoice::Specific { function, .. } => format!("function:{}", function.name),
    }
}
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use std::collections::HashMap;

pub struct OpenAIProvider {
    config: ProviderConfig,
    http_client: Client,
}

impl OpenAIProvider {
    fn base_url(&self) -> String {
        self.config
            .base_url
            .clone()
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string())
    }
}

#[async_trait]
impl Provider for OpenAIProvider {
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
        ProviderType::OpenAI
    }

    async fn chat_completions(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        stream: bool,
        tools: Option<Vec<ToolDefinition>>,
        tool_choice: Option<ToolChoice>,
    ) -> Result<ChatCompletionResponse, BackendError> {
        // Convert messages to OpenAI format
        let openai_messages: Vec<HashMap<String, serde_json::Value>> = messages
            .into_iter()
            .map(|msg| {
                let mut message = HashMap::new();
                message.insert("role".to_string(), serde_json::Value::String(msg.role));
                
                match msg.content {
                    Some(MessageContent::Text(text)) => {
                        message.insert("content".to_string(), serde_json::Value::String(text));
                    }
                    Some(MessageContent::Parts(parts)) => {
                        // Convert parts to text (simplified)
                        let text = parts
                            .into_iter()
                            .filter_map(|part| {
                                if part.r#type == "text" {
                                    part.text
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<String>>()
                            .join(" ");
                        message.insert("content".to_string(), serde_json::Value::String(text));
                    }
                    None => {
                        message.insert("content".to_string(), serde_json::Value::String("".to_string()));
                    }
                }
                
                if let Some(name) = msg.name {
                    message.insert("name".to_string(), serde_json::Value::String(name));
                }
                
                message
            })
            .collect();

        // Build request payload
        let mut payload = json!({
            "model": model,
            "messages": openai_messages,
            "stream": stream,
        });

        // Add tools if provided
        if let Some(tools) = tools {
            let tool_defs: Vec<serde_json::Value> = tools
                .into_iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": tool.function.name,
                            "description": tool.function.description,
                            "parameters": tool.function.parameters,
                        }
                    })
                })
                .collect();
            
            payload["tools"] = serde_json::Value::Array(tool_defs);
            
            // Add tool_choice if provided
            if let Some(tool_choice) = tool_choice {
                let tool_choice_value = match tool_choice {
                    ToolChoice::Mode(mode) => {
                        serde_json::Value::String(mode)
                    }
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

        // Make the request
        let response = self
            .http_client
            .post(format!("{}/chat/completions", self.base_url()))
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| BackendError::ExecutionFailed(format!("OpenAI API request error: {}", e)))?;

        let status = response.status();
        
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            return Err(BackendError::ExecutionFailed(format!(
                "OpenAI API error ({}): {}",
                status, error_text
            )));
        }

        if stream {
            // For streaming responses, we need to handle the stream
            // For now, return an error since streaming is complex
            Err(BackendError::NotAvailable(
                "Streaming not yet implemented for OpenAI provider".to_string(),
            ))
        } else {
            // Parse the response
            let response_json: serde_json::Value = response
                .json()
                .await
                .map_err(|e| BackendError::InvalidResponse(format!("Failed to parse response: {}", e)))?;

            // Convert to ChatCompletionResponse
            // This is a simplified conversion - in production we'd want full parsing
            let id = response_json["id"]
                .as_str()
                .unwrap_or("")
                .to_string();
            
            let choices = response_json["choices"]
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .map(|choice| {
                    let message = &choice["message"];
                    let content = message["content"].as_str().map(|s| s.to_string());
                    
                    // Handle tool calls
                    let tool_calls = if let Some(tool_calls_array) = message["tool_calls"].as_array() {
                        let calls: Vec<_> = tool_calls_array
                            .iter()
                            .map(|tc| {
                                crate::proxy::openai::ToolCall {
                                    id: tc["id"].as_str().unwrap_or("").to_string(),
                                    r#type: "function".to_string(),
                                    function: crate::proxy::openai::FunctionCall {
                                        name: tc["function"]["name"].as_str().unwrap_or("").to_string(),
                                        arguments: tc["function"]["arguments"].as_str().unwrap_or("").to_string(),
                                    },
                                }
                            })
                            .collect();
                        Some(calls)
                    } else {
                        None
                    };
                    
                    crate::proxy::openai::Choice {
                        index: choice["index"].as_u64().unwrap_or(0) as u32,
                        message: crate::proxy::openai::ChatMessage {
                            role: message["role"].as_str().unwrap_or("assistant").to_string(),
                            content: content.map(MessageContent::Text),
                            name: None,
                            tool_calls,
                            tool_call_id: None,
                        },
                        finish_reason: choice["finish_reason"].as_str().map(|s| s.to_string()),
                        logprobs: None,
                    }
                })
                .collect();
            
            let usage = if let Some(usage_obj) = response_json["usage"].as_object() {
                crate::proxy::openai::Usage {
                    prompt_tokens: usage_obj["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                    completion_tokens: usage_obj["completion_tokens"].as_u64().unwrap_or(0) as u32,
                    total_tokens: usage_obj["total_tokens"].as_u64().unwrap_or(0) as u32,
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