use std::sync::Mutex;

use async_trait::async_trait;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use mockito::Matcher;

use super::backend::{BackendError, ModelInfo as BackendModelInfo};
use super::gateway::{GatewayConfig, GatewayType, ProviderAdapter};
use super::handlers::chat_completions;
use super::openai::{ChatCompletionResponse, Choice, Usage};
use super::routing::{self, ProviderSwitchState};
use super::telemetry::TelemetryFanout;
use super::{ChatCompletionRequest, ProxyState};
use crate::config::{ProxyConfig, ProxyObservabilityConfig};
use crate::providers::ProviderRegistry;
use crate::providers::alloy::AlloyManager;
use crate::sync::Arc;

#[derive(Debug)]
struct RecordingGateway {
    config: GatewayConfig,
    requests: Mutex<Vec<ChatCompletionRequest>>,
}

impl RecordingGateway {
    fn new() -> Self {
        Self {
            config: GatewayConfig {
                backend_type: GatewayType::Mock,
                base_url: None,
                api_key: None,
                timeout_seconds: 30,
                extra_config: None,
                headers: None,
                retry: Default::default(),
                ui_url: None,
            },
            requests: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ProviderAdapter for RecordingGateway {
    fn gateway_type(&self) -> GatewayType {
        GatewayType::Mock
    }

    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, BackendError> {
        let model = request.model.clone();
        self.requests.lock().expect("recording mutex").push(request);
        Ok(ChatCompletionResponse {
            id: "chatcmpl-test".to_string(),
            object: "chat.completion".to_string(),
            created: 1,
            model,
            choices: vec![Choice {
                index: 0,
                message: super::openai::ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(super::openai::MessageContent::Text("ok".to_string())),
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
        })
    }

    async fn list_models(&self) -> Result<Vec<BackendModelInfo>, BackendError> {
        Ok(Vec::new())
    }

    fn config(&self) -> &GatewayConfig {
        &self.config
    }
}

#[tokio::test]
async fn provider_route_emits_gateway_attempt_telemetry_without_payloads() {
    let mut telemetry_server = mockito::Server::new_async().await;
    let telemetry_mock = telemetry_server
        .mock("POST", "/events")
        .match_body(Matcher::PartialJson(serde_json::json!({
            "event_type": "model_gateway.attempt",
            "agent_id": "agent-1",
            "requested_model": "opencode-go/kimi-k2.6",
            "root_model": "opencode-go/kimi-k2.6",
            "concrete_model": "opencode-go/kimi-k2.6",
            "upstream_model": "kimi-k2.6",
            "provider_id": "opencode-go",
            "outcome": "success",
            "message_count": 1
        })))
        .with_status(204)
        .create_async()
        .await;

    let default_gateway = Arc::new(RecordingGateway::new());
    let provider_gateway = Arc::new(RecordingGateway::new());
    let state = ProxyState {
        alloy_manager: Arc::new(AlloyManager::empty()),
        provider_registry: Arc::new(ProviderRegistry::new()),
        config: ProxyConfig {
            backend_type: "http".to_string(),
            ..Default::default()
        },
        model_shortcuts: Vec::new(),
        gateway: default_gateway,
        providers: vec![routing::ProviderEntry {
            id: "opencode-go".to_string(),
            patterns: vec!["opencode-go/kimi-k2.6".to_string()],
            gateway: provider_gateway,
            on_switch: None,
            switch_state: Arc::new(ProviderSwitchState::default()),
            strip_model_prefix: Some("opencode-go/".to_string()),
            add_model_prefix: None,
            fallback_on: ProxyConfig::default().fallback_on,
            request_body: serde_json::Map::new(),
        }],
        telemetry: TelemetryFanout::from_config(&[ProxyObservabilityConfig {
            kind: "http-json".to_string(),
            endpoint: Some(format!("{}/events", telemetry_server.url())),
            ..ProxyObservabilityConfig::default()
        }])
        .expect("telemetry fanout"),
        local_manager: None,
        voice: None,
    };

    let req: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
        "model": "opencode-go/kimi-k2.6",
        "messages": [{"role": "user", "content": "do not leak this payload"}]
    }))
    .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("x-agent-id", HeaderValue::from_static("agent-1"));
    let response = chat_completions(State(state), headers, Json(req))
        .await
        .into_response();

    assert_eq!(response.status(), StatusCode::OK);
    wait_for_mock(&telemetry_mock).await;
}

async fn wait_for_mock(mock_: &mockito::Mock) {
    for _ in 0..20 {
        if mock_.matched_async().await {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    mock_.assert_async().await;
}
