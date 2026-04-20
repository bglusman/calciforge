//! HTTP request handlers for the Alloy proxy

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response, Sse},
    Json,
};
use futures_util::stream::{self};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, error, info, warn};

use crate::providers::alloy::AlloyPlan;
use crate::proxy::{
    openai::{
        ApiError, ChatCompletionChunk, ChatCompletionResponse, ChunkChoice, DeltaMessage,
        ErrorDetail, ModelInfo, ModelListResponse,
    },
    routing, ChatCompletionRequest, ProxyState,
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
    headers: axum::http::HeaderMap,
    Json(req): Json<ChatCompletionRequest>,
) -> Response {
    debug!(model = %req.model, stream = req.stream.unwrap_or(false), "Chat completion request");

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

    // Validate model exists. Skip when:
    //  - A named provider matches (provider is authoritative for its models).
    //  - Backend is http (upstream is authoritative).
    //  - It's a configured alloy.
    let provider_matches = routing::find_provider(&state.providers, &req.model).is_some();
    let is_http_backend = state.config.backend_type == "http";
    let is_valid_model = provider_matches
        || is_http_backend
        || state.alloy_manager.get_alloy(&req.model).is_some()
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

    // Check if model is an alloy alias
    let plan = if let Some(alloy) = state.alloy_manager.get_alloy(&req.model) {
        info!(alloy_id = %req.model, "Using alloy for request");
        alloy.select_plan()
    } else {
        // Direct model reference - create single-constituent "plan"
        crate::providers::alloy::AlloyPlan {
            alloy_id: req.model.clone(),
            alloy_name: req.model.clone(),
            ordered_models: vec![req.model.clone()],
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
    // Auth check — same key as the proxy API.
    if let Some(ref expected_key) = state.config.api_key {
        let provided = headers
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .unwrap_or("");
        if provided != expected_key.as_str() {
            return api_error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Invalid API key",
                None,
            );
        }
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
pub async fn list_models(State(state): State<ProxyState>) -> impl IntoResponse {
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
            owned_by: "zeroclawed".to_string(),
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
