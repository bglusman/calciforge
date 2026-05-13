use super::backend::{BackendConfig, BackendError, BackendType, create_backend};
use super::gateway::*;
use super::openai::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, Choice, MessageContent, Usage,
};
use crate::config::GatewayRetryConfig;
use mockito::Matcher;
use std::collections::HashMap;

#[test]
fn test_gateway_type_parsing() {
    assert_eq!(
        "helicone".parse::<GatewayType>().unwrap(),
        GatewayType::Helicone
    );
    assert_eq!(
        "direct".parse::<GatewayType>().unwrap(),
        GatewayType::BuiltinHttp
    );
    assert_eq!(
        "http".parse::<GatewayType>().unwrap(),
        GatewayType::BuiltinHttp
    );
    assert_eq!(
        "builtin-http".parse::<GatewayType>().unwrap(),
        GatewayType::BuiltinHttp
    );
    assert_eq!(
        "litellm".parse::<GatewayType>().unwrap(),
        GatewayType::LiteLlm
    );
    assert_eq!(
        "lite-llm".parse::<GatewayType>().unwrap(),
        GatewayType::LiteLlm
    );
    assert_eq!(
        "portkey".parse::<GatewayType>().unwrap(),
        GatewayType::Portkey
    );
    assert_eq!(
        "tensor-zero".parse::<GatewayType>().unwrap(),
        GatewayType::TensorZero
    );
    assert_eq!(
        "futureagi".parse::<GatewayType>().unwrap(),
        GatewayType::FutureAgi
    );
    assert_eq!(
        "open-router".parse::<GatewayType>().unwrap(),
        GatewayType::OpenRouter
    );
    assert_eq!("mock".parse::<GatewayType>().unwrap(), GatewayType::Mock);
    assert!("unknown".parse::<GatewayType>().is_err());
}

#[test]
fn test_gateway_type_display() {
    assert_eq!(GatewayType::Helicone.to_string(), "helicone");
    assert_eq!(GatewayType::BuiltinHttp.to_string(), "builtin-http");
    assert_eq!(GatewayType::LiteLlm.to_string(), "litellm");
    assert_eq!(GatewayType::Portkey.to_string(), "portkey");
    assert_eq!(GatewayType::TensorZero.to_string(), "tensorzero");
    assert_eq!(GatewayType::FutureAgi.to_string(), "future-agi");
    assert_eq!(GatewayType::OpenRouter.to_string(), "openrouter");
    assert_eq!(GatewayType::Mock.to_string(), "mock");
}

#[test]
fn named_gateway_engines_share_openai_compatible_http_core() {
    for gateway_type in [
        GatewayType::Helicone,
        GatewayType::BuiltinHttp,
        GatewayType::LiteLlm,
        GatewayType::Portkey,
        GatewayType::TensorZero,
        GatewayType::FutureAgi,
        GatewayType::OpenRouter,
    ] {
        assert!(
            gateway_type.uses_openai_compatible_http_core(),
            "{gateway_type} should use the shared OpenAI-compatible HTTP core"
        );
    }
    assert!(!GatewayType::Mock.uses_openai_compatible_http_core());
}

#[test]
fn helicone_policy_headers_are_overlay_not_separate_gateway_core() {
    let retry = GatewayRetryConfig {
        enabled: true,
        max_retries: 4,
        min_timeout_ms: 250,
        max_timeout_ms: 3_000,
        factor: 3,
        retry_on: vec![],
    };

    let headers = openai_compatible_headers(GatewayType::Helicone, Some("test-key"), &retry, None)
        .expect("helicone overlay should add headers");

    assert_eq!(
        headers.get("helicone-auth"),
        Some(&"Bearer test-key".to_string())
    );
    assert_eq!(
        headers.get("helicone-retry-enabled"),
        Some(&"true".to_string())
    );
    assert_eq!(headers.get("helicone-retry-num"), Some(&"4".to_string()));
    assert_eq!(
        headers.get("helicone-retry-min-timeout"),
        Some(&"250".to_string())
    );
    assert_eq!(
        headers.get("helicone-retry-max-timeout"),
        Some(&"3000".to_string())
    );
    assert_eq!(headers.get("helicone-retry-factor"), Some(&"3".to_string()));

    assert!(
        openai_compatible_headers(
            GatewayType::LiteLlm,
            Some("test-key"),
            &GatewayRetryConfig::default(),
            None
        )
        .is_none(),
        "LiteLLM should not inherit Helicone-specific headers"
    );
}

#[test]
fn test_mock_gateway() {
    let config = GatewayConfig {
        backend_type: GatewayType::Mock,
        base_url: None,
        api_key: None,
        timeout_seconds: 30,
        extra_config: None,
        headers: None,
        retry: GatewayRetryConfig::default(),
        ui_url: None,
    };

    let gateway = MockGateway::new(config);
    assert_eq!(gateway.gateway_type(), GatewayType::Mock);
}

#[tokio::test]
async fn mock_gateway_returns_openai_compatible_chat_choice() {
    let gateway = MockGateway::new(GatewayConfig {
        backend_type: GatewayType::Mock,
        ..Default::default()
    });

    let response = gateway
        .chat_completion(ChatCompletionRequest {
            model: "gpt-4".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("short".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_content: None,
            }],
            max_tokens: Some(2),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(response.model, "gpt-4");
    let choice = response
        .choices
        .first()
        .expect("mock gateway should return an assistant choice");
    assert_eq!(choice.message.role, "assistant");
    let Some(MessageContent::Text(content)) = choice.message.content.as_ref() else {
        panic!("mock gateway choice should contain text content");
    };
    assert!(
        content.contains("gpt-4") && content.to_lowercase().contains("mock"),
        "mock response content should identify the routed model: {content}"
    );
}

#[test]
fn gateway_engine_info_carries_operator_ui_link() {
    let config = GatewayConfig {
        backend_type: GatewayType::Helicone,
        ui_url: Some("http://127.0.0.1:8585".to_string()),
        ..Default::default()
    };

    let info = config.engine_info(GatewayType::Helicone);

    assert_eq!(info.id, "helicone");
    assert_eq!(info.display_name, "Helicone AI Gateway");
    assert_eq!(info.ui_url.as_deref(), Some("http://127.0.0.1:8585"));
    assert!(info.capabilities.operator_ui);
    assert!(info.capabilities.observability);
    assert!(!info.capabilities.model_listing);
    assert!(!info.capabilities.tool_call_transcripts);
    assert!(!info.capabilities.config_validation);
}

#[test]
fn builtin_http_gateway_retries_only_configured_failure_kinds() {
    let retry = GatewayRetryConfig {
        enabled: true,
        max_retries: 2,
        min_timeout_ms: 1,
        max_timeout_ms: 10,
        factor: 2,
        retry_on: vec![crate::config::GatewayFailureKind::ServerError],
    };
    let server_error =
        BackendError::http_status_error(reqwest::StatusCode::SERVICE_UNAVAILABLE, "down");
    let auth_error = BackendError::http_status_error(reqwest::StatusCode::UNAUTHORIZED, "bad key");

    assert!(should_retry_locally(
        GatewayType::BuiltinHttp,
        &retry,
        &server_error,
        0
    ));
    assert!(!should_retry_locally(
        GatewayType::BuiltinHttp,
        &retry,
        &auth_error,
        0
    ));
    assert!(!should_retry_locally(
        GatewayType::BuiltinHttp,
        &retry,
        &server_error,
        2
    ));
}

#[test]
fn helicone_retry_policy_is_not_applied_twice_locally() {
    let retry = GatewayRetryConfig {
        enabled: true,
        max_retries: 2,
        min_timeout_ms: 1,
        max_timeout_ms: 10,
        factor: 2,
        retry_on: vec![crate::config::GatewayFailureKind::ServerError],
    };
    let server_error =
        BackendError::http_status_error(reqwest::StatusCode::SERVICE_UNAVAILABLE, "down");

    assert!(
        !should_retry_locally(GatewayType::Helicone, &retry, &server_error, 0),
        "Helicone receives retry headers, so Calciforge must not multiply attempts locally"
    );
}

#[tokio::test]
async fn builtin_http_gateway_forwards_complete_chat_request_options() {
    let mut server = mockito::Server::new_async().await;
    let response = ChatCompletionResponse {
        id: "chatcmpl-test".to_string(),
        object: "chat.completion".to_string(),
        created: 1,
        model: "kimi-for-coding".to_string(),
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
        .match_header("x-client-family", "kimi-cli")
        .match_body(Matcher::PartialJson(serde_json::json!({
            "model": "kimi-for-coding",
            "max_tokens": 16,
            "temperature": 0.5,
            "thinking": {"type": "enabled"},
            "stream": false,
            "messages": [{"role": "user", "content": "hello"}]
        })))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(serde_json::to_string(&response).unwrap())
        .create_async()
        .await;

    let mut headers = HashMap::new();
    headers.insert("x-client-family".to_string(), "kimi-cli".to_string());
    let backend = create_backend(&BackendConfig {
        backend_type: BackendType::Http,
        url: Some(format!("{}/v1", server.url())),
        api_key: Some("provider-key".to_string()),
        timeout_seconds: Some(30),
        headers: Some(headers.clone()),
    })
    .unwrap();
    let gateway = create_gateway(
        GatewayConfig {
            backend_type: GatewayType::BuiltinHttp,
            base_url: Some(format!("{}/v1", server.url())),
            api_key: Some("provider-key".to_string()),
            timeout_seconds: 30,
            headers: Some(headers),
            ..Default::default()
        },
        Some(backend),
    )
    .unwrap();

    let result = gateway
        .chat_completion(
            serde_json::from_value(serde_json::json!({
                "model": "kimi-for-coding",
                "messages": [{"role": "user", "content": "hello"}],
                "max_tokens": 16,
                "temperature": 0.5,
                "stream": false,
                "thinking": {"type": "enabled"}
            }))
            .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(result.model, "kimi/kimi-for-coding");
    mock.assert_async().await;
}

#[tokio::test]
async fn configured_authorization_header_cannot_override_backend_api_key() {
    let mut server = mockito::Server::new_async().await;
    let response = ChatCompletionResponse {
        id: "chatcmpl-auth-order".to_string(),
        object: "chat.completion".to_string(),
        created: 1,
        model: "managed/default".to_string(),
        choices: vec![Choice {
            index: 0,
            message: ChatMessage {
                role: "assistant".to_string(),
                content: Some(MessageContent::Text("pong".to_string())),
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
        .match_header("authorization", "Bearer backend-key")
        .match_header("x-provider-boundary", "litellm")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(serde_json::to_string(&response).unwrap())
        .create_async()
        .await;

    let mut headers = HashMap::new();
    headers.insert(
        "Authorization".to_string(),
        "Bearer wrong-configured-key".to_string(),
    );
    headers.insert("x-provider-boundary".to_string(), "litellm".to_string());
    let backend = create_backend(&BackendConfig {
        backend_type: BackendType::Http,
        url: Some(format!("{}/v1", server.url())),
        api_key: Some("backend-key".to_string()),
        timeout_seconds: Some(30),
        headers: Some(headers),
    })
    .unwrap();
    let gateway = create_gateway(
        GatewayConfig {
            backend_type: GatewayType::LiteLlm,
            base_url: Some(format!("{}/v1", server.url())),
            api_key: Some("backend-key".to_string()),
            timeout_seconds: 30,
            ..Default::default()
        },
        Some(backend),
    )
    .unwrap();

    gateway
        .chat_completion(
            serde_json::from_value(serde_json::json!({
                "model": "managed/default",
                "messages": [{"role": "user", "content": "ping"}]
            }))
            .unwrap(),
        )
        .await
        .unwrap();

    mock.assert_async().await;
}

#[test]
fn create_openai_compatible_gateway_preserves_engine_metadata_through_logging_wrapper() {
    let backend = create_backend(&BackendConfig {
        backend_type: BackendType::Http,
        url: Some("http://127.0.0.1:8787/v1".to_string()),
        api_key: Some("helicone-test-key".to_string()),
        timeout_seconds: Some(30),
        ..Default::default()
    })
    .unwrap();
    let gateway = create_gateway(
        GatewayConfig {
            backend_type: GatewayType::Helicone,
            base_url: Some("https://ai-gateway.helicone.ai".to_string()),
            api_key: Some("helicone-test-key".to_string()),
            ui_url: Some("https://us.helicone.ai/requests".to_string()),
            ..Default::default()
        },
        Some(backend),
    )
    .unwrap();

    let info = gateway.engine_info();

    assert_eq!(gateway.gateway_type(), GatewayType::Helicone);
    assert_eq!(info.id, "helicone");
    assert_eq!(info.display_name, "Helicone AI Gateway");
    assert_eq!(
        info.ui_url.as_deref(),
        Some("https://us.helicone.ai/requests")
    );
    assert!(info.capabilities.openai_chat_completions);
    assert!(info.capabilities.operator_ui);
    assert!(info.capabilities.observability);
}

#[tokio::test]
async fn helicone_engine_uses_shared_http_core_with_engine_headers() {
    let mut server = mockito::Server::new_async().await;
    let response = ChatCompletionResponse {
        id: "chatcmpl-test".to_string(),
        object: "chat.completion".to_string(),
        created: 1,
        model: "ollama/qwen3.6:27b".to_string(),
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
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(serde_json::to_string(&response).unwrap())
        .create_async()
        .await;

    let headers = openai_compatible_headers(
        GatewayType::Helicone,
        Some("helicone-test-key"),
        &GatewayRetryConfig::default(),
        None,
    );
    let backend = create_backend(&BackendConfig {
        backend_type: BackendType::Http,
        url: Some(format!("{}/v1", server.url())),
        api_key: Some("helicone-test-key".to_string()),
        timeout_seconds: Some(30),
        headers: headers.clone(),
    })
    .unwrap();
    let gateway = create_gateway(
        GatewayConfig {
            backend_type: GatewayType::Helicone,
            base_url: Some(format!("{}/v1/", server.url())),
            api_key: Some("helicone-test-key".to_string()),
            timeout_seconds: 30,
            headers,
            ..Default::default()
        },
        Some(backend),
    )
    .unwrap();

    let result = gateway
        .chat_completion(ChatCompletionRequest {
            model: "ollama/qwen3.6:27b".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("hello".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_content: None,
            }],
            max_tokens: Some(16),
            temperature: Some(0.2),
            stream: Some(false),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(result.model, "ollama/qwen3.6:27b");
    mock.assert_async().await;
}

#[tokio::test]
async fn litellm_engine_parses_streaming_response_from_shared_http_core() {
    let mut server = mockito::Server::new_async().await;
    let body = concat!(
        "data: {\"id\":\"chatcmpl-litellm-stream\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"qwen3.6\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"po\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-litellm-stream\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"qwen3.6\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ng\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
        "data: [DONE]\n\n",
    );
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream; charset=utf-8")
        .with_body(body)
        .create_async()
        .await;

    let backend = create_backend(&BackendConfig {
        backend_type: BackendType::Http,
        url: Some(format!("{}/v1", server.url())),
        timeout_seconds: Some(30),
        ..Default::default()
    })
    .unwrap();
    let gateway = create_gateway(
        GatewayConfig {
            backend_type: GatewayType::LiteLlm,
            base_url: Some(format!("{}/v1", server.url())),
            timeout_seconds: 30,
            ..Default::default()
        },
        Some(backend),
    )
    .unwrap();

    let result = gateway
        .chat_completion(ChatCompletionRequest {
            model: "qwen3.6".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text("hello".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                reasoning_content: None,
            }],
            stream: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(result.model, "qwen3.6");
    assert_eq!(
        result.choices[0]
            .message
            .content
            .as_ref()
            .and_then(MessageContent::to_text)
            .as_deref(),
        Some("pong")
    );
    mock.assert_async().await;
}
