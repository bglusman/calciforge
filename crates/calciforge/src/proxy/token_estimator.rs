//! Token estimation for model-gateway context-window routing.
//!
//! Dispatchers and cascades need a conservative "will this request fit?"
//! answer before choosing a target model. These estimators are intentionally
//! for routing safety, not billing-grade token accounting.

use super::openai::{ChatCompletionRequest, ChatMessage, MessageContent, ToolDefinition};

const DEFAULT_OUTPUT_BUDGET: u32 = 4096;

/// Estimate token counts for context-window fit checks.
pub trait TokenEstimator: Send + Sync {
    /// Estimate tokens for plain text. Implementations should over-estimate
    /// slightly so fit checks fail closed into a larger model.
    fn estimate_text(&self, text: &str) -> u32;

    /// Estimate input tokens in an OpenAI-style chat request.
    fn estimate_chat_input(&self, req: &ChatCompletionRequest) -> u32 {
        let mut total = 16u32;
        for message in &req.messages {
            total = total.saturating_add(self.estimate_message(message));
        }
        if let Some(tools) = &req.tools {
            total = total.saturating_add(self.estimate_tools(tools));
        }
        total
    }

    /// Estimate total context pressure: input plus requested output budget.
    fn estimate_chat_request(&self, req: &ChatCompletionRequest) -> u32 {
        self.estimate_chat_input(req)
            .saturating_add(req.max_tokens.unwrap_or(DEFAULT_OUTPUT_BUDGET))
    }

    fn estimate_message(&self, message: &ChatMessage) -> u32 {
        let mut total = self.estimate_text(&message.role).saturating_add(4);
        if let Some(content) = &message.content {
            total = total.saturating_add(self.estimate_content(content));
        }
        if let Some(reasoning) = &message.reasoning {
            total = total.saturating_add(self.estimate_text(reasoning));
        }
        if let Some(reasoning_content) = &message.reasoning_content {
            total = total.saturating_add(self.estimate_text(reasoning_content));
        }
        if let Some(tool_calls) = &message.tool_calls {
            total = total.saturating_add(self.estimate_json(tool_calls));
        }
        total
    }

    fn estimate_content(&self, content: &MessageContent) -> u32 {
        match content {
            MessageContent::Text(text) => self.estimate_text(text),
            MessageContent::Parts(parts) => parts.iter().fold(0u32, |acc, part| {
                let text_tokens = part
                    .text
                    .as_ref()
                    .map(|text| self.estimate_text(text))
                    .unwrap_or_default();
                let image_tokens = if part.image_url.is_some() { 512 } else { 0 };
                acc.saturating_add(text_tokens).saturating_add(image_tokens)
            }),
        }
    }

    fn estimate_tools(&self, tools: &[ToolDefinition]) -> u32 {
        tools.iter().fold(0u32, |acc, tool| {
            acc.saturating_add(self.estimate_json(tool))
        })
    }

    fn estimate_json<T: serde::Serialize>(&self, value: &T) -> u32 {
        serde_json::to_string(value)
            .map(|json| self.estimate_text(&json))
            .unwrap_or_default()
    }
}

/// Configurable characters-per-token estimator.
#[derive(Debug, Clone, Copy)]
pub struct CharRatioEstimator {
    pub chars_per_token: f32,
    pub safety_margin: f32,
}

impl Default for CharRatioEstimator {
    fn default() -> Self {
        Self {
            chars_per_token: 3.5,
            safety_margin: 1.10,
        }
    }
}

impl TokenEstimator for CharRatioEstimator {
    fn estimate_text(&self, text: &str) -> u32 {
        estimate_ratio(
            text.chars().count(),
            self.chars_per_token,
            self.safety_margin,
        )
    }
}

/// Byte-oriented estimator for denser tokenizer families or mixed-language
/// prompts where character counts are too optimistic.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct ByteRatioEstimator {
    pub bytes_per_token: f32,
    pub safety_margin: f32,
}

impl Default for ByteRatioEstimator {
    fn default() -> Self {
        Self {
            bytes_per_token: 3.0,
            safety_margin: 1.15,
        }
    }
}

impl TokenEstimator for ByteRatioEstimator {
    fn estimate_text(&self, text: &str) -> u32 {
        estimate_ratio(text.len(), self.bytes_per_token, self.safety_margin)
    }
}

fn estimate_ratio(units: usize, units_per_token: f32, safety_margin: f32) -> u32 {
    let divisor = if units_per_token.is_finite() && units_per_token > 0.0 {
        units_per_token
    } else {
        1.0
    };
    let margin = if safety_margin.is_finite() && safety_margin > 0.0 {
        safety_margin
    } else {
        1.0
    };
    let estimate = ((units as f32 / divisor) * margin).ceil();
    if estimate >= u32::MAX as f32 {
        u32::MAX
    } else {
        estimate as u32
    }
}

pub fn default_request_estimate(req: &ChatCompletionRequest) -> u32 {
    CharRatioEstimator::default().estimate_chat_request(req)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::openai::{ChatCompletionRequest, ChatMessage, MessageContent};

    fn request(content: &str, max_tokens: Option<u32>) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "local/small".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text(content.to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_content: None,
            }],
            max_tokens,
            ..Default::default()
        }
    }

    #[test]
    fn char_ratio_estimator_applies_safety_margin() {
        let est = CharRatioEstimator {
            chars_per_token: 4.0,
            safety_margin: 1.25,
        };
        assert_eq!(est.estimate_text("abcdefgh"), 3);
    }

    #[test]
    fn byte_ratio_estimator_counts_utf8_bytes() {
        let est = ByteRatioEstimator {
            bytes_per_token: 3.0,
            safety_margin: 1.0,
        };
        assert_eq!(est.estimate_text("火火"), 2);
    }

    #[test]
    fn request_estimate_reserves_output_budget() {
        let est = CharRatioEstimator::default();
        let without_explicit_budget = est.estimate_chat_request(&request("hello", None));
        let with_small_budget = est.estimate_chat_request(&request("hello", Some(16)));
        assert!(without_explicit_budget >= with_small_budget + 4000);
    }
}
