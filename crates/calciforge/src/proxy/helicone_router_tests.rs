use std::collections::HashMap;

use super::helicone_router::{HeliconeRouter, HeliconeRouterConfig, helicone_chat_completions_url};
use super::openai::{ChatCompletionResponse, ChatMessage, Choice, MessageContent, Usage};
use crate::config::GatewayRetryConfig;
use mockito::Matcher;

fn config(base_url: String) -> HeliconeRouterConfig {
    HeliconeRouterConfig {
        base_url,
        api_key: "helicone-test-key".to_string(),
        timeout_seconds: 30,
        router_name: "test".to_string(),
        enable_caching: false,
        cache_ttl_seconds: 300,
        headers: HashMap::new(),
        retry: GatewayRetryConfig::default(),
    }
}

#[test]
fn test_helicone_router_creation() {
    let router = HeliconeRouter::new(config("http://localhost:8787".to_string()));
    assert!(router.is_ok());
}

#[test]
fn test_default_router() {
    let router = HeliconeRouter::default();
    assert!(router.is_ok());
}

#[test]
fn helicone_url_adds_v1_path_for_origin_base() {
    let url = helicone_chat_completions_url("https://ai-gateway.helicone.ai").unwrap();
    assert_eq!(
        url.as_str(),
        "https://ai-gateway.helicone.ai/v1/chat/completions"
    );
}

#[test]
fn helicone_url_uses_configured_gateway_base_path() {
    let url = helicone_chat_completions_url("https://gateway.example.invalid/router/calciforge/")
        .unwrap();
    assert_eq!(
        url.as_str(),
        "https://gateway.example.invalid/router/calciforge/chat/completions"
    );
}

#[test]
fn helicone_url_does_not_duplicate_v1_path() {
    let url = helicone_chat_completions_url("https://ai-gateway.helicone.ai/v1/").unwrap();
    assert_eq!(
        url.as_str(),
        "https://ai-gateway.helicone.ai/v1/chat/completions"
    );
}

#[test]
fn helicone_url_rejects_query_or_fragment_base() {
    let err = helicone_chat_completions_url("https://ai-gateway.helicone.ai/v1?debug=true")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("query parameters or fragments"),
        "unexpected error: {err}"
    );

    let err = helicone_chat_completions_url("https://ai-gateway.helicone.ai/v1#dashboard")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("query parameters or fragments"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn chat_completion_posts_to_configured_v1_path_without_duplication() {
    let mut server = mockito::Server::new_async().await;
    let response = ChatCompletionResponse {
        id: "chatcmpl-test".to_string(),
        object: "chat.completion".to_string(),
        created: 1,
        model: "openai/gpt-4o-mini".to_string(),
        choices: vec![Choice {
            index: 0,
            message: ChatMessage {
                role: "assistant".to_string(),
                content: Some(MessageContent::Text("ok".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_content: None,
            },
            finish_reason: Some("stop".to_string()),
            logprobs: None,
        }],
        usage: Usage {
            prompt_tokens: 1,
            completion_tokens: 1,
            total_tokens: 2,
        },
        system_fingerprint: None,
    };
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .match_header("authorization", "Bearer helicone-test-key")
        .match_header("helicone-auth", "Bearer helicone-test-key")
        .match_body(Matcher::PartialJson(serde_json::json!({
            "model": "openai/gpt-4o-mini",
            "messages": [{"role": "user", "content": "hello"}]
        })))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(serde_json::to_string(&response).unwrap())
        .create_async()
        .await;

    let router = HeliconeRouter::new(config(format!("{}/v1/", server.url()))).unwrap();
    let result = router
        .chat_completion(
            "openai/gpt-4o-mini".to_string(),
            vec![ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("hello".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_content: None,
            }],
            false,
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(result.model, "openai/gpt-4o-mini");
    mock.assert_async().await;
}

#[tokio::test]
async fn chat_completion_forwards_custom_headers() {
    let mut server = mockito::Server::new_async().await;
    let response = ChatCompletionResponse {
        id: "chatcmpl-test".to_string(),
        object: "chat.completion".to_string(),
        created: 1,
        model: "openai/gpt-4o-mini".to_string(),
        choices: vec![Choice {
            index: 0,
            message: ChatMessage {
                role: "assistant".to_string(),
                content: Some(MessageContent::Text("ok".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_content: None,
            },
            finish_reason: Some("stop".to_string()),
            logprobs: None,
        }],
        usage: Usage {
            prompt_tokens: 1,
            completion_tokens: 1,
            total_tokens: 2,
        },
        system_fingerprint: None,
    };
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .match_header("authorization", "Bearer helicone-test-key")
        .match_header("helicone-auth", "Bearer helicone-test-key")
        .match_header("x-provider-scope", "local-ollama")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(serde_json::to_string(&response).unwrap())
        .create_async()
        .await;

    let mut cfg = config(format!("{}/v1/", server.url()));
    cfg.headers
        .insert("x-provider-scope".to_string(), "local-ollama".to_string());
    cfg.headers
        .insert("authorization".to_string(), "Bearer wrong".to_string());
    cfg.headers
        .insert("helicone-auth".to_string(), "Bearer wrong".to_string());
    let router = HeliconeRouter::new(cfg).unwrap();
    router
        .chat_completion(
            "openai/gpt-4o-mini".to_string(),
            vec![ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("hello".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_content: None,
            }],
            false,
            None,
            None,
        )
        .await
        .unwrap();

    mock.assert_async().await;
}

#[tokio::test]
async fn chat_completion_maps_retry_config_to_helicone_headers() {
    let mut server = mockito::Server::new_async().await;
    let response = ChatCompletionResponse {
        id: "chatcmpl-test".to_string(),
        object: "chat.completion".to_string(),
        created: 1,
        model: "openai/gpt-4o-mini".to_string(),
        choices: vec![Choice {
            index: 0,
            message: ChatMessage {
                role: "assistant".to_string(),
                content: Some(MessageContent::Text("ok".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_content: None,
            },
            finish_reason: Some("stop".to_string()),
            logprobs: None,
        }],
        usage: Usage {
            prompt_tokens: 1,
            completion_tokens: 1,
            total_tokens: 2,
        },
        system_fingerprint: None,
    };
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .match_header("helicone-retry-enabled", "true")
        .match_header("helicone-retry-num", "4")
        .match_header("helicone-retry-min-timeout", "250")
        .match_header("helicone-retry-max-timeout", "3000")
        .match_header("helicone-retry-factor", "3")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(serde_json::to_string(&response).unwrap())
        .create_async()
        .await;

    let mut cfg = config(format!("{}/v1/", server.url()));
    cfg.retry.enabled = true;
    cfg.retry.max_retries = 4;
    cfg.retry.min_timeout_ms = 250;
    cfg.retry.max_timeout_ms = 3000;
    cfg.retry.factor = 3;
    let router = HeliconeRouter::new(cfg).unwrap();
    router
        .chat_completion(
            "openai/gpt-4o-mini".to_string(),
            vec![ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("hello".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_content: None,
            }],
            false,
            None,
            None,
        )
        .await
        .unwrap();

    mock.assert_async().await;
}

#[tokio::test]
async fn chat_completion_error_names_gateway_and_model_without_full_body_dump() {
    let mut server = mockito::Server::new_async().await;
    let long_body = format!("{}{}", "denied: ", "x".repeat(2048));
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(503)
        .with_header("content-type", "text/plain")
        .with_body(long_body)
        .create_async()
        .await;

    let router = HeliconeRouter::new(config(format!("{}/v1/", server.url()))).unwrap();
    let err = router
        .chat_completion(
            "openai/gpt-4o-mini".to_string(),
            vec![ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("hello".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_content: None,
            }],
            false,
            None,
            None,
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("503 Service Unavailable"), "{err}");
    assert!(err.contains("openai/gpt-4o-mini"), "{err}");
    assert!(err.contains("denied:"), "{err}");
    assert!(
        err.len() < 1300,
        "error should be truncated instead of dumping full upstream body: {} bytes",
        err.len()
    );
    mock.assert_async().await;
}
