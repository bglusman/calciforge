//! HTTP request handlers for the model gateway

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response, Sse},
    Json,
};
use futures_util::stream::{self};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, error, info, warn};

use crate::config::ProxyConfig;
use crate::providers::alloy::AlloyPlan;
use crate::proxy::{
    openai::{
        ApiError, ChatCompletionChunk, ChatCompletionResponse, ChunkChoice, DeltaMessage,
        ErrorDetail, ModelInfo, ModelListResponse,
    },
    routing, token_estimator, ChatCompletionRequest, ProxyState,
};

/// List of valid/known models - in production this would come from config or backend
const KNOWN_MODELS: &[&str] = &[
    "deepseek-chat",
    "deepseek-reasoner",
    "gpt-4",
    "gpt-4o",
    "claude-3-5-sonnet",
    "claude-3-opus",
    "kimi-free",
    "kimi/kimi-for-coding",
    "kimi-for-coding",
];

/// Handler for POST /v1/chat/completions
pub async fn chat_completions(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    Json(req): Json<ChatCompletionRequest>,
) -> Response {
    debug!(model = %req.model, stream = req.stream.unwrap_or(false), "Chat completion request");

    if let Some(response) = require_api_key(&state.config, &headers) {
        return response;
    }

    // Extract agent ID from header
    let agent_id = headers
        .get("x-agent-id")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("anonymous");

    // Check model access for this agent
    if !crate::proxy::auth::check_model_access(&state.config, agent_id, &req.model) {
        warn!(agent_id = %agent_id, model = %req.model, "Model access denied");
        return api_error(
            StatusCode::FORBIDDEN,
            "model_access_denied",
            &format!(
                "Agent '{}' does not have access to model '{}'",
                agent_id, req.model
            ),
            None,
        );
    }

    let estimated_tokens = token_estimator::estimate_request(&req, &state.config.token_estimator);

    // Validate model exists. Skip when:
    //  - A named provider matches (provider is authoritative for its models).
    //  - Backend is http (upstream is authoritative).
    //  - It's a configured synthetic model (alloy, cascade, dispatcher, exec model).
    let provider_matches = routing::find_provider(&state.providers, &req.model).is_some();
    let is_http_backend = state.config.backend_type == "http";
    let is_valid_model = provider_matches
        || is_http_backend
        || state.alloy_manager.is_synthetic_model(&req.model)
        || KNOWN_MODELS.contains(&req.model.as_str());

    if !is_valid_model {
        warn!(model = %req.model, "Invalid model requested");
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_model",
            &format!("Model '{}' is not available", req.model),
            Some("model"),
        );
    }

    let plan = match state
        .alloy_manager
        .select_plan_for_model(&req.model, estimated_tokens)
    {
        Ok(Some(plan)) => {
            info!(
                synthetic_model = %req.model,
                estimated_tokens,
                attempts = plan.ordered_models.len(),
                "Using synthetic model plan for request"
            );
            plan
        }
        Ok(None) => crate::providers::alloy::AlloyPlan {
            alloy_id: req.model.clone(),
            alloy_name: req.model.clone(),
            ordered_models: vec![req.model.clone()],
        },
        Err(e) => {
            warn!(model = %req.model, estimated_tokens, error = %e, "Synthetic model cannot fit request");
            return api_error(
                StatusCode::BAD_REQUEST,
                "context_window_exceeded",
                &e,
                Some("messages"),
            );
        }
    };

    // Route to provider with fallback
    let result = route_with_fallback(&state, &plan, &req).await;

    match result {
        Ok(response) => {
            if req.should_stream() {
                streaming_response(response, &plan.alloy_name).await
            } else {
                Json(response).into_response()
            }
        }
        Err(_) => {
            // Log detailed error internally but return generic message to client
            // This prevents leaking backend details (security fix)
            error!("Request failed after all fallbacks - see server logs for details");
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                "The requested model is temporarily unavailable. Please try again later.",
                None,
            )
        }
    }
}

/// Route request with fallback chain
async fn route_with_fallback(
    state: &ProxyState,
    plan: &AlloyPlan,
    req: &ChatCompletionRequest,
) -> anyhow::Result<ChatCompletionResponse> {
    let mut last_error = None;

    for (idx, model) in plan.ordered_models.iter().enumerate() {
        debug!(attempt = idx + 1, model = %model, "Trying constituent");

        // Record attempt in stats
        state
            .alloy_manager
            .record_attempt(&plan.alloy_id, model, true);

        match try_provider(state, model, req).await {
            Ok(response) => {
                info!(model = %model, "Request succeeded");
                return Ok(response);
            }
            Err(e) => {
                warn!(model = %model, error = %e, "Provider failed, trying fallback");
                // Update attempt to failure
                state
                    .alloy_manager
                    .record_attempt(&plan.alloy_id, model, false);
                last_error = Some(e);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("No constituents available")))
}

/// Try a single provider, selecting the appropriate gateway by model name.
async fn try_provider(
    state: &ProxyState,
    model: &str,
    req: &ChatCompletionRequest,
) -> anyhow::Result<ChatCompletionResponse> {
    let mut gateway_req = req.clone();
    gateway_req.model = model.to_string();

    // Check named providers first; fall back to default gateway.
    let gateway = routing::find_provider(&state.providers, model)
        .map(|e| &e.gateway)
        .unwrap_or(&state.gateway);

    match gateway.chat_completion(gateway_req).await {
        Ok(response) => Ok(response),
        Err(e) => {
            anyhow::bail!("Gateway error: {}", e);
        }
    }
}

/// Handler for POST /control/local/switch
pub async fn local_model_switch(
    State(state): State<ProxyState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Some(response) = require_api_key(&state.config, &headers) {
        return response;
    }

    let model_id = match body.get("model").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "Request body must include {\"model\": \"<id>\"}",
                Some("model"),
            );
        }
    };

    let manager = match state.local_manager {
        Some(ref m) => m.clone(),
        None => {
            return api_error(
                StatusCode::NOT_FOUND,
                "local_models_disabled",
                "Local model management is not configured (add [local_models] to config)",
                None,
            );
        }
    };

    // Run the blocking switch on a blocking thread (avoids blocking the async runtime).
    let model_id_clone = model_id.clone();
    let result = tokio::task::spawn_blocking(move || manager.switch(&model_id_clone)).await;

    match result {
        Ok(Ok(loaded)) => {
            info!(model = %loaded.id, "Local model switch succeeded");
            Json(serde_json::json!({
                "status": "ok",
                "model": loaded.id,
                "hf_id": loaded.hf_id,
            }))
            .into_response()
        }
        Ok(Err(e)) => {
            warn!(model = %model_id, error = %e, "Local model switch failed");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "switch_failed",
                &format!("Model switch failed: {e}"),
                None,
            )
        }
        Err(e) => {
            error!(error = %e, "spawn_blocking panic during model switch");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Internal error during model switch",
                None,
            )
        }
    }
}

/// Handler for GET /v1/models
pub async fn list_models(State(state): State<ProxyState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_api_key(&state.config, &headers) {
        return response;
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut models = vec![];

    // Add configured local models (if any).
    if let Some(ref mgr) = state.local_manager {
        let current = mgr.current();
        for m in mgr.models() {
            let suffix = if current.as_ref().map(|c| c.id.as_str()) == Some(&m.id) {
                " (loaded)"
            } else {
                ""
            };
            models.push(ModelInfo {
                id: m.id.clone(),
                object: "model".to_string(),
                created: now,
                owned_by: format!("local/{}{}", m.provider_type, suffix),
            });
        }
    }

    // Add configured alloys
    for alloy in state.alloy_manager.list_alloys() {
        models.push(ModelInfo {
            id: alloy.definition().id.clone(),
            object: "model".to_string(),
            created: now,
            owned_by: "calciforge".to_string(),
        });
    }
    for cascade in state.alloy_manager.list_cascades() {
        models.push(ModelInfo {
            id: cascade.id.clone(),
            object: "model".to_string(),
            created: now,
            owned_by: "calciforge/cascade".to_string(),
        });
    }
    for dispatcher in state.alloy_manager.list_dispatchers() {
        models.push(ModelInfo {
            id: dispatcher.id.clone(),
            object: "model".to_string(),
            created: now,
            owned_by: "calciforge/dispatcher".to_string(),
        });
    }
    for exec_model in state.alloy_manager.list_exec_models() {
        models.push(ModelInfo {
            id: exec_model.id.clone(),
            object: "model".to_string(),
            created: now,
            owned_by: "calciforge/exec".to_string(),
        });
    }

    // Try to get models from gateway
    match state.gateway.list_models().await {
        Ok(gateway_models) => {
            for model_info in gateway_models {
                models.push(ModelInfo {
                    id: model_info.id,
                    object: "model".to_string(),
                    created: now,
                    owned_by: model_info.provider.unwrap_or_else(|| "unknown".to_string()),
                });
            }
        }
        Err(e) => {
            warn!(error = %e, "Failed to get models from gateway, using fallback");
            // Fallback to hardcoded models
            models.push(ModelInfo {
                id: "gpt-4".to_string(),
                object: "model".to_string(),
                created: now,
                owned_by: "openai".to_string(),
            });
            models.push(ModelInfo {
                id: "claude-3-5-sonnet".to_string(),
                object: "model".to_string(),
                created: now,
                owned_by: "anthropic".to_string(),
            });
            models.push(ModelInfo {
                id: "deepseek-chat".to_string(),
                object: "model".to_string(),
                created: now,
                owned_by: "deepseek".to_string(),
            });
            models.push(ModelInfo {
                id: "kimi-free".to_string(),
                object: "model".to_string(),
                created: now,
                owned_by: "kimi".to_string(),
            });
            models.push(ModelInfo {
                id: "kimi/kimi-for-coding".to_string(),
                object: "model".to_string(),
                created: now,
                owned_by: "kimi".to_string(),
            });
        }
    }

    Json(ModelListResponse {
        object: "list".to_string(),
        data: models,
    })
    .into_response()
}

/// Handler for GET /health
pub async fn health_check() -> impl IntoResponse {
    Json(json!({
        "status": "healthy",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Create a streaming SSE response
async fn streaming_response(response: ChatCompletionResponse, model: &str) -> Response {
    let id = response.id.clone();
    let created = response.created;
    let model = model.to_string();

    // Create a simple stream with the response
    let stream = stream::iter(vec![
        Ok::<_, std::convert::Infallible>(
            axum::response::sse::Event::default().data(
                serde_json::to_string(&ChatCompletionChunk {
                    id: id.clone(),
                    object: "chat.completion.chunk".to_string(),
                    created,
                    model: model.clone(),
                    system_fingerprint: None,
                    choices: vec![ChunkChoice {
                        index: 0,
                        delta: DeltaMessage {
                            role: Some("assistant".to_string()),
                            content: None,
                            tool_calls: None,
                        },
                        finish_reason: None,
                        logprobs: None,
                    }],
                })
                .unwrap(),
            ),
        ),
        Ok(axum::response::sse::Event::default().data(
            serde_json::to_string(&ChatCompletionChunk {
                id: id.clone(),
                object: "chat.completion.chunk".to_string(),
                created,
                model: model.clone(),
                system_fingerprint: None,
                choices: vec![ChunkChoice {
                    index: 0,
                    delta: DeltaMessage {
                        role: None,
                        content: response
                            .choices
                            .first()
                            .and_then(|c| c.message.content.as_ref().and_then(|c| c.to_text())),
                        tool_calls: response
                            .choices
                            .first()
                            .and_then(|c| c.message.tool_calls.clone()),
                    },
                    finish_reason: None,
                    logprobs: None,
                }],
            })
            .unwrap(),
        )),
        Ok(axum::response::sse::Event::default().data(
            serde_json::to_string(&ChatCompletionChunk {
                id: id.clone(),
                object: "chat.completion.chunk".to_string(),
                created,
                model: model.clone(),
                system_fingerprint: None,
                choices: vec![ChunkChoice {
                    index: 0,
                    delta: DeltaMessage::default(),
                    finish_reason: response
                        .choices
                        .first()
                        .and_then(|c| c.finish_reason.clone()),
                    logprobs: None,
                }],
            })
            .unwrap(),
        )),
        Ok(axum::response::sse::Event::default().data("[DONE]")),
    ]);

    Sse::new(stream).into_response()
}

/// Helper to create an API error response
fn api_error(status: StatusCode, error_type: &str, message: &str, param: Option<&str>) -> Response {
    let error = ApiError {
        error: ErrorDetail {
            message: message.to_string(),
            r#type: error_type.to_string(),
            param: param.map(|s| s.to_string()),
            code: Some(error_type.to_string()),
        },
    };

    (status, Json(error)).into_response()
}

fn require_api_key(config: &ProxyConfig, headers: &HeaderMap) -> Option<Response> {
    let expected_key = config.api_key.as_deref()?.trim();
    if expected_key.is_empty() {
        return None;
    }

    let provided = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| {
            let trimmed = s.trim();
            let mut parts = trimmed.splitn(2, char::is_whitespace);
            let scheme = parts.next()?;
            let token = parts.next()?.trim();
            if scheme.eq_ignore_ascii_case("Bearer") && !token.is_empty() {
                Some(token)
            } else {
                None
            }
        });

    if provided == Some(expected_key) {
        None
    } else {
        Some(api_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Invalid API key",
            None,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn config_with_key(key: Option<&str>) -> ProxyConfig {
        ProxyConfig {
            api_key: key.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn require_api_key_treats_empty_configured_key_as_disabled() {
        let headers = HeaderMap::new();
        assert!(require_api_key(&config_with_key(Some("  ")), &headers).is_none());
    }

    #[test]
    fn require_api_key_accepts_case_insensitive_bearer_scheme() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("bearer test-key"));
        assert!(require_api_key(&config_with_key(Some("test-key")), &headers).is_none());
    }

    #[test]
    fn require_api_key_accepts_trailing_bearer_token_whitespace() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer test-key  "),
        );
        assert!(require_api_key(&config_with_key(Some("test-key")), &headers).is_none());
    }

    #[test]
    fn require_api_key_rejects_missing_bearer_token() {
        let headers = HeaderMap::new();
        let response = require_api_key(&config_with_key(Some("test-key")), &headers);
        assert_eq!(response.unwrap().status(), StatusCode::UNAUTHORIZED);
    }
}
