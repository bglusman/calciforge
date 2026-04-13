//! HTTP request handlers for the Alloy proxy

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response, Sse},
    Json,
};
use futures_util::stream::{self};
use std::time::{SystemTime, UNIX_EPOCH};
use serde_json::json;
use tracing::{info, warn, error, debug};

use crate::proxy::{
    ProxyState, ChatCompletionRequest,
    openai::{
        ChatCompletionResponse, ChatCompletionChunk, DeltaMessage, ChunkChoice, ModelListResponse, ModelInfo, 
        ApiError, ErrorDetail,
    },
};
use crate::providers::alloy::AlloyPlan;

/// List of valid/known models - in production this would come from config or backend
const KNOWN_MODELS: &[&str] = &[
    "deepseek-chat",
    "deepseek-reasoner",
    "gpt-4",
    "gpt-4o",
    "claude-3-5-sonnet",
    "claude-3-opus",
    "kimi-free",
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
            &format!("Agent '{}' does not have access to model '{}'", agent_id, req.model),
            None,
        );
    }

    // Validate model exists (either as alloy or known model)
    let is_valid_model = state.alloy_manager.get_alloy(&req.model).is_some()
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
        state.alloy_manager.record_attempt(&plan.alloy_id, model, true);

        match try_provider(state, model, req).await {
            Ok(response) => {
                info!(model = %model, "Request succeeded");
                return Ok(response);
            }
            Err(e) => {
                warn!(model = %model, error = %e, "Provider failed, trying fallback");
                // Update attempt to failure
                state.alloy_manager.record_attempt(&plan.alloy_id, model, false);
                last_error = Some(e);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("No constituents available")))
}

/// Try a single provider
async fn try_provider(
    state: &ProxyState,
    model: &str,
    req: &ChatCompletionRequest,
) -> anyhow::Result<ChatCompletionResponse> {
    // Create a request with the specific model
    let mut gateway_req = req.clone();
    gateway_req.model = model.to_string();
    
    // Use the gateway
    match state.gateway.chat_completion(gateway_req).await {
        Ok(response) => Ok(response),
        Err(e) => {
            anyhow::bail!("Gateway error: {}", e);
        }
    }
}

/// Handler for GET /v1/models
pub async fn list_models(
    State(state): State<ProxyState>,
) -> impl IntoResponse {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Build model list from alloys + direct providers
    let mut models = vec![];

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
async fn streaming_response(
    response: ChatCompletionResponse,
    model: &str,
) -> Response {
    let id = response.id.clone();
    let created = response.created;
    let model = model.to_string();
    
    // Create a simple stream with the response
    let stream = stream::iter(vec![Ok::<_, std::convert::Infallible>(
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
            }).unwrap()
        )
    ), Ok(
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
                        role: None,
                        content: response.choices.first().and_then(|c| c.message.content.as_ref().and_then(|c| c.to_text())),
                        tool_calls: None,
                    },
                    finish_reason: None,
                    logprobs: None,
                }],
            }).unwrap()
        )
    ), Ok(
        axum::response::sse::Event::default().data(
            serde_json::to_string(&ChatCompletionChunk {
                id: id.clone(),
                object: "chat.completion.chunk".to_string(),
                created,
                model: model.clone(),
                system_fingerprint: None,
                choices: vec![ChunkChoice {
                    index: 0,
                    delta: DeltaMessage::default(),
                    finish_reason: response.choices.first().and_then(|c| c.finish_reason.clone()),
                    logprobs: None,
                }],
            }).unwrap()
        )
    ), Ok(
        axum::response::sse::Event::default().data("[DONE]")
    )]);

    Sse::new(stream).into_response()
}

/// Helper to create an API error response
fn api_error(
    status: StatusCode,
    error_type: &str,
    message: &str,
    param: Option<&str>,
) -> Response {
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
