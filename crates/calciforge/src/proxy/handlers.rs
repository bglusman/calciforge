//! HTTP request handlers for the model gateway

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response, Sse},
};
use futures_util::stream::{self};
use serde_json::json;
use std::collections::HashSet;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{debug, error, info, warn};

use crate::config::ProxyConfig;
use crate::model_names::is_exact_model_pattern;
use crate::proxy::backend::BackendError;
use crate::proxy::{
    ChatCompletionRequest, ProxyState,
    model_resolver::ModelResolver,
    openai::{
        ApiError, ChatCompletionChunk, ChatCompletionResponse, ChunkChoice, DeltaMessage,
        ErrorDetail, ModelInfo, ModelListResponse,
    },
    routing, token_estimator,
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
const PROVIDER_ON_SWITCH_TIMEOUT: Duration = Duration::from_secs(120);

/// Handler for POST /v1/chat/completions
pub async fn chat_completions(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    Json(mut req): Json<ChatCompletionRequest>,
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

    let requested_model = req.model.clone();
    let model_resolver = ModelResolver::new(&state.model_shortcuts, &state.alloy_manager);
    let resolved_request_model = match model_resolver.resolve_alias_chain(&requested_model) {
        Ok(model) => model,
        Err(e) => {
            warn!(model = %requested_model, error = %e, "Model shortcut resolution failed");
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_model_alias",
                &e,
                Some("model"),
            );
        }
    };
    if resolved_request_model != requested_model {
        debug!(
            requested_model = %requested_model,
            resolved_model = %resolved_request_model,
            "Resolved proxy model shortcut"
        );
        req.model = resolved_request_model;
    }

    // Check top-level model access for this agent. Concrete constituent access
    // is checked after the synthetic plan is fully expanded below.
    if !crate::proxy::auth::check_model_access_for_names(
        &state.config,
        agent_id,
        &requested_model,
        &req.model,
    ) {
        warn!(agent_id = %agent_id, requested_model = %requested_model, model = %req.model, "Model access denied");
        return api_error(
            StatusCode::FORBIDDEN,
            "model_access_denied",
            &format!(
                "Agent '{}' does not have access to model '{}'",
                agent_id, requested_model
            ),
            None,
        );
    }

    let estimated_tokens = token_estimator::estimate_request(&req, &state.config.token_estimator);

    // Validate model exists. Skip when:
    //  - A named provider matches (provider is authoritative for its models).
    //  - Backend delegates model selection to an external OpenAI-compatible API.
    //  - It's a configured synthetic selector (alloy, cascade, dispatcher).
    let provider_matches = routing::find_provider(&state.providers, &req.model).is_some();
    let is_valid_model = provider_matches
        || super::backend_accepts_unlisted_models(&state.config.backend_type)
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

    let resolved = match model_resolver.plan_for_model(&req.model, estimated_tokens) {
        Ok(resolved) => {
            info!(
                synthetic_model = %req.model,
                estimated_tokens,
                attempts = resolved.plan.ordered_models.len(),
                "Resolved model plan for request"
            );
            resolved
        }
        Err(e) => {
            warn!(model = %req.model, estimated_tokens, error = %e, "Model plan cannot serve request");
            let (code, param) = model_plan_error_response(&e);
            return api_error(StatusCode::BAD_REQUEST, code, &e, param);
        }
    };

    for concrete_model in &resolved.plan.ordered_models {
        if !crate::proxy::auth::check_model_access_for_names(
            &state.config,
            agent_id,
            &resolved.root_model,
            concrete_model,
        ) {
            warn!(
                agent_id = %agent_id,
                requested_model = %requested_model,
                root_model = %resolved.root_model,
                concrete_model = %concrete_model,
                "Expanded model access denied"
            );
            return api_error(
                StatusCode::FORBIDDEN,
                "model_access_denied",
                &format!(
                    "Agent '{}' does not have access to model '{}'",
                    agent_id, requested_model
                ),
                None,
            );
        }
    }

    // Route to provider with fallback
    let result = route_with_fallback(&state, &resolved.plan, &req).await;

    match result {
        Ok(response) => {
            if req.should_stream() {
                streaming_response(response, &resolved.plan.alloy_name).await
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

fn model_plan_error_response(error: &str) -> (&'static str, Option<&'static str>) {
    if error.contains("shortcut cycle") {
        ("invalid_model_alias", Some("model"))
    } else if error.contains("synthetic model cycle") {
        ("invalid_model_plan", Some("model"))
    } else if error.contains("context window") || error.contains("estimated request size") {
        ("context_window_exceeded", Some("messages"))
    } else {
        ("invalid_model_plan", Some("model"))
    }
}

/// Return operator-facing metadata for the active gateway engine.
pub async fn gateway_info(State(state): State<ProxyState>) -> Response {
    Json(state.gateway.engine_info()).into_response()
}

/// Redirect to the configured gateway UI/dashboard when one is available.
pub async fn gateway_ui_redirect(State(state): State<ProxyState>) -> Response {
    match state.gateway.engine_info().ui_url {
        Some(url) if !url.trim().is_empty() => Redirect::temporary(url.trim()).into_response(),
        _ => api_error(
            StatusCode::NOT_FOUND,
            "gateway_ui_unavailable",
            "No gateway UI URL is configured for this Calciforge gateway.",
            None,
        ),
    }
}

/// Route request with fallback chain
async fn route_with_fallback(
    state: &ProxyState,
    plan: &crate::providers::alloy::AlloyPlan,
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
                let failure_kind = e.failure_kind();
                let fallback_on = fallback_policy_for_model(state, model);
                let can_fallback =
                    idx + 1 < plan.ordered_models.len() && fallback_on.contains(&failure_kind);
                warn!(
                    model = %model,
                    error = %e,
                    ?failure_kind,
                    can_fallback,
                    "Provider failed"
                );
                // Update attempt to failure
                state
                    .alloy_manager
                    .record_attempt(&plan.alloy_id, model, false);
                last_error = Some(anyhow::anyhow!("Gateway error: {}", e));
                if !can_fallback {
                    break;
                }
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
) -> Result<ChatCompletionResponse, BackendError> {
    // Check named providers first; fall back to default gateway.
    let provider = routing::find_provider(&state.providers, model);
    let gateway = provider
        .map(|entry| &entry.gateway)
        .unwrap_or(&state.gateway);
    let mut gateway_req = req.clone();
    gateway_req.model = provider
        .map(|entry| entry.upstream_model_name(model))
        .unwrap_or_else(|| model.to_string());

    if let Some(provider) = provider {
        gateway_req.extra_body.extend(provider.request_body.clone());
        ensure_provider_model_ready(provider, model, &gateway_req.model)
            .await
            .map_err(|e| BackendError::ConfigError(e.to_string()))?;
    }

    gateway.chat_completion(gateway_req).await
}

fn fallback_policy_for_model(
    state: &ProxyState,
    model: &str,
) -> Vec<crate::config::GatewayFailureKind> {
    routing::find_provider(&state.providers, model)
        .map(|entry| entry.fallback_on.clone())
        .unwrap_or_else(|| state.config.fallback_on.clone())
}

async fn ensure_provider_model_ready(
    provider: &routing::ProviderEntry,
    model: &str,
    upstream_model: &str,
) -> anyhow::Result<()> {
    let Some(script) = provider
        .on_switch
        .as_deref()
        .map(str::trim)
        .filter(|script| !script.is_empty())
    else {
        return Ok(());
    };

    let mut current = provider.switch_state.lock().await;
    if current.as_deref() == Some(model) {
        return Ok(());
    }
    let previous = current.clone().unwrap_or_default();
    let provider_id = provider.id.clone();
    let model_id = model.to_string();
    let upstream_model_id = upstream_model.to_string();
    let script = script.to_string();

    tokio::task::spawn_blocking(move || {
        run_provider_on_switch_script(
            &script,
            &provider_id,
            &model_id,
            &upstream_model_id,
            &previous,
        )
    })
    .await
    .map_err(|e| anyhow::anyhow!("provider on_switch task failed: {e}"))?
    .map_err(|e| anyhow::anyhow!("provider on_switch failed: {e}"))?;

    *current = Some(model.to_string());
    Ok(())
}

fn run_provider_on_switch_script(
    script: &str,
    provider_id: &str,
    model_id: &str,
    upstream_model_id: &str,
    previous: &str,
) -> anyhow::Result<()> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(script)
        .env("CALCIFORGE_PROVIDER_ID", provider_id)
        .env("CALCIFORGE_MODEL_ID", model_id)
        .env("CALCIFORGE_UPSTREAM_MODEL_ID", upstream_model_id)
        .env("CALCIFORGE_PREV_MODEL_ID", previous)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("provider on_switch spawn failed: {e}"))?;

    let start = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| anyhow::anyhow!("provider on_switch wait failed: {e}"))?
        {
            if status.success() {
                return Ok(());
            }
            anyhow::bail!("provider on_switch exited with status {status}");
        }
        if start.elapsed() >= PROVIDER_ON_SWITCH_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!(
                "provider on_switch exceeded {}s timeout",
                PROVIDER_ON_SWITCH_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
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

/// Handler for GET /control/secrets/list
pub async fn secret_list(State(state): State<ProxyState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_control_api_key(&state.config, &headers) {
        return response;
    }

    match secrets_client::FnoxClient::new().list().await {
        Ok(secrets) => match secrets_client::metadata::metadata_for_names(&secrets) {
            Ok(metadata) => (
                StatusCode::OK,
                Json(json!({ "secrets": secrets, "metadata": metadata })),
            )
                .into_response(),
            Err(err) => api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "secret_metadata_unavailable",
                &format!("secret metadata unavailable: {err}"),
                None,
            ),
        },
        Err(err) => api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "secret_store_unavailable",
            &format!("fnox list failed: {err}"),
            None,
        ),
    }
}

/// Handler for GET /control/secrets/ref/:name
pub async fn secret_reference(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Some(response) = require_control_api_key(&state.config, &headers) {
        return response;
    }

    match secrets_client::secret_reference_token(&name) {
        Some(reference) => {
            (StatusCode::OK, Json(json!({ "reference": reference }))).into_response()
        }
        None => api_error(
            StatusCode::BAD_REQUEST,
            "invalid_secret_name",
            "Secret names may contain only A-Z, a-z, 0-9, underscore, and dash",
            Some("name"),
        ),
    }
}

/// Handler for POST /control/secrets/set
pub async fn secret_set(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if let Some(response) = require_control_api_key(&state.config, &headers) {
        return response;
    }

    let name = match body.get("name").and_then(|v| v.as_str()) {
        Some(name) => name,
        None => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "Request body must include {\"name\":\"...\",\"value\":\"...\"}",
                Some("name"),
            );
        }
    };
    if secrets_client::secret_reference_token(name).is_none() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_secret_name",
            "Secret names may contain only A-Z, a-z, 0-9, underscore, and dash",
            Some("name"),
        );
    }

    let value = match body.get("value").and_then(|v| v.as_str()) {
        Some(value) if !value.is_empty() => value,
        _ => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "Request body must include a non-empty string value",
                Some("value"),
            );
        }
    };

    let allowed_destinations_present = body.get("allowed_destinations").is_some();
    let allowed_destinations = match body.get("allowed_destinations") {
        Some(value) => match serde_json::from_value::<Vec<String>>(value.clone()) {
            Ok(values) => match secrets_client::metadata::parse_destinations(&values.join(",")) {
                Ok(values) => values,
                Err(err) => {
                    return api_error(
                        StatusCode::BAD_REQUEST,
                        "invalid_allowed_destinations",
                        &err.to_string(),
                        Some("allowed_destinations"),
                    );
                }
            },
            Err(_) => {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    "allowed_destinations must be an array of host patterns",
                    Some("allowed_destinations"),
                );
            }
        },
        None => Vec::new(),
    };

    match secrets_client::FnoxClient::new().set(name, value).await {
        Ok(()) => {
            if allowed_destinations_present
                && let Err(err) =
                    secrets_client::metadata::set_allowed_destinations(name, &allowed_destinations)
            {
                return api_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "secret_metadata_unavailable",
                    &format!("secret value stored, but failed to store destination policy: {err}"),
                    None,
                );
            }
            (StatusCode::OK, Json(json!({ "stored": name }))).into_response()
        }
        Err(err) => api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "secret_store_unavailable",
            &format!("fnox set failed: {err}"),
            None,
        ),
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
    let mut seen_model_ids = HashSet::new();

    let mut push_model = |models: &mut Vec<ModelInfo>, model: ModelInfo| {
        if seen_model_ids.insert(model.id.clone()) {
            models.push(model);
        }
    };

    // Add client-facing model shortcuts. They are valid model IDs for gateway
    // requests, so `/v1/models` should advertise them as selectable aliases.
    for shortcut in &state.model_shortcuts {
        push_model(
            &mut models,
            ModelInfo {
                id: shortcut.alias.clone(),
                object: "model".to_string(),
                created: now,
                owned_by: format!("calciforge/shortcut -> {}", shortcut.model),
            },
        );
    }

    // Add configured local models (if any).
    if let Some(ref mgr) = state.local_manager {
        let current = mgr.current();
        for m in mgr.models() {
            let suffix = if current.as_ref().map(|c| c.id.as_str()) == Some(&m.id) {
                " (loaded)"
            } else {
                ""
            };
            push_model(
                &mut models,
                ModelInfo {
                    id: m.id.clone(),
                    object: "model".to_string(),
                    created: now,
                    owned_by: format!("local/{}{}", m.provider_type, suffix),
                },
            );
        }
    }

    // Add configured alloys
    for alloy in state.alloy_manager.list_alloys() {
        push_model(
            &mut models,
            ModelInfo {
                id: alloy.definition().id.clone(),
                object: "model".to_string(),
                created: now,
                owned_by: "calciforge".to_string(),
            },
        );
    }
    for cascade in state.alloy_manager.list_cascades() {
        push_model(
            &mut models,
            ModelInfo {
                id: cascade.id.clone(),
                object: "model".to_string(),
                created: now,
                owned_by: "calciforge/cascade".to_string(),
            },
        );
    }
    for dispatcher in state.alloy_manager.list_dispatchers() {
        push_model(
            &mut models,
            ModelInfo {
                id: dispatcher.id.clone(),
                object: "model".to_string(),
                created: now,
                owned_by: "calciforge/dispatcher".to_string(),
            },
        );
    }
    // Add exact configured provider route IDs. Some backends, including local
    // Helicone AI Gateway deployments, may not return a useful upstream model
    // list even though Calciforge can route exact configured model IDs. This is
    // intentionally after Calciforge-owned models so aliases, local models, and
    // synthetic definitions keep their more specific ownership metadata.
    for provider in &state.providers {
        for pattern in &provider.patterns {
            if !is_exact_model_pattern(pattern) {
                continue;
            }
            push_model(
                &mut models,
                ModelInfo {
                    id: pattern.clone(),
                    object: "model".to_string(),
                    created: now,
                    owned_by: format!("calciforge/provider:{}", provider.id),
                },
            );
        }
    }

    // Try to get models from gateway
    match state.gateway.list_models().await {
        Ok(gateway_models) => {
            for model_info in gateway_models {
                push_model(
                    &mut models,
                    ModelInfo {
                        id: model_info.id,
                        object: "model".to_string(),
                        created: now,
                        owned_by: model_info.provider.unwrap_or_else(|| "unknown".to_string()),
                    },
                );
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

fn require_control_api_key(config: &ProxyConfig, headers: &HeaderMap) -> Option<Response> {
    let Some(expected_key) = config.api_key.as_deref().map(str::trim) else {
        return Some(api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "control_api_auth_not_configured",
            "Secret control API requires proxy.api_key or proxy.api_key_file",
            None,
        ));
    };
    if expected_key.is_empty() {
        return Some(api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "control_api_auth_not_configured",
            "Secret control API requires a non-empty proxy.api_key or proxy.api_key_file",
            None,
        ));
    }

    require_api_key(config, headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        DispatcherConfig, ModelShortcutConfig, ProxyAccessPolicy, ProxyAgentConfig,
        SyntheticModelConfig,
    };
    use crate::providers::ProviderRegistry;
    use crate::providers::alloy::AlloyManager;
    use crate::proxy::ProxyState;
    use crate::proxy::backend::{BackendError, ModelInfo as BackendModelInfo};
    use crate::proxy::gateway::{GatewayBackend, GatewayConfig, GatewayType};
    use crate::proxy::openai::{ChatCompletionResponse, Choice, Usage};
    use crate::sync::Arc;
    use async_trait::async_trait;
    use axum::body::to_bytes;
    use axum::http::HeaderValue;
    use serde_json::Value;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[derive(Debug)]
    struct RecordingGateway {
        config: GatewayConfig,
        requests: Mutex<Vec<ChatCompletionRequest>>,
        failures: Mutex<VecDeque<BackendError>>,
    }

    impl RecordingGateway {
        fn new() -> Self {
            Self {
                config: GatewayConfig {
                    backend_type: GatewayType::BuiltinHttp,
                    base_url: None,
                    api_key: None,
                    timeout_seconds: 30,
                    extra_config: None,
                    headers: None,
                    retry: crate::config::GatewayRetryConfig::default(),
                    ui_url: None,
                },
                requests: Mutex::new(Vec::new()),
                failures: Mutex::new(VecDeque::new()),
            }
        }

        fn with_failures(failures: Vec<BackendError>) -> Self {
            let gateway = Self::new();
            *gateway
                .failures
                .lock()
                .expect("recording gateway mutex poisoned") = failures.into();
            gateway
        }

        fn recorded_models(&self) -> Vec<String> {
            self.requests
                .lock()
                .expect("recording gateway mutex poisoned")
                .iter()
                .map(|request| request.model.clone())
                .collect()
        }

        fn recorded_requests(&self) -> Vec<ChatCompletionRequest> {
            self.requests
                .lock()
                .expect("recording gateway mutex poisoned")
                .clone()
        }
    }

    #[async_trait]
    impl GatewayBackend for RecordingGateway {
        fn gateway_type(&self) -> GatewayType {
            GatewayType::BuiltinHttp
        }

        async fn chat_completion(
            &self,
            request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, BackendError> {
            let model = request.model.clone();
            self.requests
                .lock()
                .expect("recording gateway mutex poisoned")
                .push(request);
            if let Some(error) = self
                .failures
                .lock()
                .expect("recording gateway mutex poisoned")
                .pop_front()
            {
                return Err(error);
            }
            Ok(ChatCompletionResponse {
                id: "chatcmpl-test".to_string(),
                object: "chat.completion".to_string(),
                created: 1,
                model,
                choices: vec![Choice {
                    index: 0,
                    message: crate::proxy::openai::ChatMessage {
                        role: "assistant".to_string(),
                        content: Some(crate::proxy::openai::MessageContent::Text("ok".to_string())),
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

    fn config_with_key(key: Option<&str>) -> ProxyConfig {
        ProxyConfig {
            api_key: key.map(str::to_string),
            ..Default::default()
        }
    }

    fn gateway_state(gateway: Arc<dyn GatewayBackend>, config: ProxyConfig) -> ProxyState {
        ProxyState {
            alloy_manager: Arc::new(AlloyManager::empty()),
            provider_registry: Arc::new(ProviderRegistry::new()),
            config,
            model_shortcuts: Vec::new(),
            gateway,
            providers: Vec::new(),
            local_manager: None,
            voice: None,
        }
    }

    fn request_for_model(model: &str) -> ChatCompletionRequest {
        serde_json::from_value(serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "Hi"}]
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn synthetic_fallback_stops_on_auth_failure_by_default() {
        let recording_gateway = Arc::new(RecordingGateway::with_failures(vec![
            BackendError::http_status_error(
                reqwest::StatusCode::UNAUTHORIZED,
                "upstream unauthorized",
            ),
        ]));
        let state = gateway_state(recording_gateway.clone(), ProxyConfig::default());
        let plan = crate::providers::alloy::AlloyPlan {
            alloy_id: "auth-stop".to_string(),
            alloy_name: "auth-stop".to_string(),
            ordered_models: vec!["bad-auth".to_string(), "fallback".to_string()],
        };

        let result = route_with_fallback(&state, &plan, &request_for_model("auth-stop")).await;

        assert!(result.is_err(), "auth failures must not silently fallback");
        assert_eq!(recording_gateway.recorded_models(), vec!["bad-auth"]);
    }

    #[tokio::test]
    async fn synthetic_fallback_advances_on_retryable_server_failure() {
        let recording_gateway = Arc::new(RecordingGateway::with_failures(vec![
            BackendError::http_status_error(
                reqwest::StatusCode::SERVICE_UNAVAILABLE,
                "temporarily unavailable",
            ),
        ]));
        let state = gateway_state(recording_gateway.clone(), ProxyConfig::default());
        let plan = crate::providers::alloy::AlloyPlan {
            alloy_id: "server-fallback".to_string(),
            alloy_name: "server-fallback".to_string(),
            ordered_models: vec!["unavailable".to_string(), "fallback".to_string()],
        };

        let result = route_with_fallback(&state, &plan, &request_for_model("server-fallback"))
            .await
            .expect("server errors should permit fallback by default");

        assert_eq!(result.model, "fallback");
        assert_eq!(
            recording_gateway.recorded_models(),
            vec!["unavailable", "fallback"]
        );
    }

    #[tokio::test]
    async fn provider_fallback_policy_can_disable_synthetic_fallback() {
        let provider_gateway = Arc::new(RecordingGateway::with_failures(vec![
            BackendError::http_status_error(
                reqwest::StatusCode::SERVICE_UNAVAILABLE,
                "temporarily unavailable",
            ),
        ]));
        let default_gateway = Arc::new(RecordingGateway::new());
        let state = ProxyState {
            alloy_manager: Arc::new(AlloyManager::empty()),
            provider_registry: Arc::new(ProviderRegistry::new()),
            config: ProxyConfig::default(),
            model_shortcuts: Vec::new(),
            gateway: default_gateway,
            providers: vec![routing::ProviderEntry {
                id: "no-fallback".to_string(),
                patterns: vec!["*".to_string()],
                gateway: provider_gateway.clone(),
                on_switch: None,
                switch_state: Arc::new(routing::ProviderSwitchState::default()),
                strip_model_prefix: None,
                add_model_prefix: None,
                fallback_on: Vec::new(),
                request_body: serde_json::Map::new(),
            }],
            local_manager: None,
            voice: None,
        };
        let plan = crate::providers::alloy::AlloyPlan {
            alloy_id: "server-no-fallback".to_string(),
            alloy_name: "server-no-fallback".to_string(),
            ordered_models: vec!["unavailable".to_string(), "fallback".to_string()],
        };

        let result =
            route_with_fallback(&state, &plan, &request_for_model("server-no-fallback")).await;

        assert!(
            result.is_err(),
            "empty provider fallback policy should make retryable failures fatal for that provider"
        );
        assert_eq!(provider_gateway.recorded_models(), vec!["unavailable"]);
    }

    #[tokio::test]
    async fn global_fallback_policy_can_disable_default_gateway_fallback() {
        let recording_gateway = Arc::new(RecordingGateway::with_failures(vec![
            BackendError::http_status_error(
                reqwest::StatusCode::SERVICE_UNAVAILABLE,
                "temporarily unavailable",
            ),
        ]));
        let state = gateway_state(
            recording_gateway.clone(),
            ProxyConfig {
                fallback_on: Vec::new(),
                ..Default::default()
            },
        );
        let plan = crate::providers::alloy::AlloyPlan {
            alloy_id: "server-no-global-fallback".to_string(),
            alloy_name: "server-no-global-fallback".to_string(),
            ordered_models: vec!["unavailable".to_string(), "fallback".to_string()],
        };

        let result = route_with_fallback(
            &state,
            &plan,
            &request_for_model("server-no-global-fallback"),
        )
        .await;

        assert!(
            result.is_err(),
            "empty global fallback policy should make retryable failures fatal on the default gateway"
        );
        assert_eq!(recording_gateway.recorded_models(), vec!["unavailable"]);
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

    #[test]
    fn require_control_api_key_fails_closed_without_configured_key() {
        let headers = HeaderMap::new();

        let missing = require_control_api_key(&config_with_key(None), &headers)
            .expect("control API must reject unconfigured auth");
        assert_eq!(missing.status(), StatusCode::SERVICE_UNAVAILABLE);

        let empty = require_control_api_key(&config_with_key(Some("  ")), &headers)
            .expect("control API must reject empty auth");
        assert_eq!(empty.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn require_control_api_key_accepts_valid_bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer test-key"));

        assert!(require_control_api_key(&config_with_key(Some("test-key")), &headers).is_none());
    }

    #[test]
    fn model_plan_error_response_distinguishes_config_cycles_from_context_limits() {
        assert_eq!(
            model_plan_error_response(
                "synthetic model cycle detected after shortcut resolution: balanced -> balanced"
            ),
            ("invalid_model_plan", Some("model"))
        );
        assert_eq!(
            model_plan_error_response(
                "model shortcut cycle detected while resolving 'a': a -> b -> a"
            ),
            ("invalid_model_alias", Some("model"))
        );
        assert_eq!(
            model_plan_error_response(
                "dispatcher 'balanced': estimated request size 900 tokens exceeds largest configured context window 500"
            ),
            ("context_window_exceeded", Some("messages"))
        );
    }

    #[tokio::test]
    async fn proxy_resolves_model_shortcut_dispatcher_before_provider_routing() {
        let alloy_manager = AlloyManager::from_gateway_configs(
            &[],
            &[],
            &[DispatcherConfig {
                id: "local-cloud-balanced".to_string(),
                name: Some("Local/cloud balanced".to_string()),
                models: vec![
                    SyntheticModelConfig {
                        model: "qwen-test:small".to_string(),
                        context_window: 60_000,
                    },
                    SyntheticModelConfig {
                        model: "kimi-test:medium".to_string(),
                        context_window: 250_000,
                    },
                ],
            }],
        )
        .unwrap();

        let recording_gateway = Arc::new(RecordingGateway::new());
        let gateway: Arc<dyn GatewayBackend> = recording_gateway.clone();
        let state = ProxyState {
            alloy_manager: Arc::new(alloy_manager),
            provider_registry: Arc::new(ProviderRegistry::new()),
            config: ProxyConfig {
                backend_type: "http".to_string(),
                ..Default::default()
            },
            model_shortcuts: vec![ModelShortcutConfig {
                alias: "local-dispatcher".to_string(),
                model: "local-cloud-balanced".to_string(),
            }],
            gateway,
            providers: Vec::new(),
            local_manager: None,
            voice: None,
        };

        let req: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "local-dispatcher",
            "messages": [{"role": "user", "content": "Hi"}]
        }))
        .unwrap();
        let response = chat_completions(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["model"], "qwen-test:small");
        assert_eq!(recording_gateway.recorded_models(), vec!["qwen-test:small"]);
    }

    #[tokio::test]
    async fn proxy_resolves_direct_dispatcher_id_before_gateway_request() {
        let alloy_manager = AlloyManager::from_gateway_configs(
            &[],
            &[],
            &[DispatcherConfig {
                id: "local-dispatcher".to_string(),
                name: Some("Local dispatcher".to_string()),
                models: vec![
                    SyntheticModelConfig {
                        model: "qwen-test:small".to_string(),
                        context_window: 60_000,
                    },
                    SyntheticModelConfig {
                        model: "kimi-test:medium".to_string(),
                        context_window: 250_000,
                    },
                ],
            }],
        )
        .unwrap();

        let recording_gateway = Arc::new(RecordingGateway::new());
        let gateway: Arc<dyn GatewayBackend> = recording_gateway.clone();
        let state = ProxyState {
            alloy_manager: Arc::new(alloy_manager),
            provider_registry: Arc::new(ProviderRegistry::new()),
            config: ProxyConfig {
                backend_type: "http".to_string(),
                ..Default::default()
            },
            model_shortcuts: Vec::new(),
            gateway,
            providers: Vec::new(),
            local_manager: None,
            voice: None,
        };

        let req: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "local-dispatcher",
            "messages": [{"role": "user", "content": "Hi"}]
        }))
        .unwrap();
        let response = chat_completions(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(recording_gateway.recorded_models(), vec!["qwen-test:small"]);
    }

    #[tokio::test]
    async fn proxy_routes_resolved_synthetic_constituent_to_matching_provider_gateway() {
        let alloy_manager = AlloyManager::from_gateway_configs(
            &[],
            &[],
            &[DispatcherConfig {
                id: "local-dispatcher".to_string(),
                name: Some("Local dispatcher".to_string()),
                models: vec![SyntheticModelConfig {
                    model: "qwen-test:small".to_string(),
                    context_window: 60_000,
                }],
            }],
        )
        .unwrap();

        let default_gateway = Arc::new(RecordingGateway::new());
        let provider_gateway = Arc::new(RecordingGateway::new());
        let default_gateway_dyn: Arc<dyn GatewayBackend> = default_gateway.clone();
        let provider_gateway_dyn: Arc<dyn GatewayBackend> = provider_gateway.clone();
        let state = ProxyState {
            alloy_manager: Arc::new(alloy_manager),
            provider_registry: Arc::new(ProviderRegistry::new()),
            config: ProxyConfig {
                backend_type: "helicone".to_string(),
                ..Default::default()
            },
            model_shortcuts: vec![ModelShortcutConfig {
                alias: "balanced".to_string(),
                model: "local-dispatcher".to_string(),
            }],
            gateway: default_gateway_dyn,
            providers: vec![routing::ProviderEntry {
                id: "helicone-local".to_string(),
                patterns: vec!["qwen-test:small".to_string()],
                gateway: provider_gateway_dyn,
                on_switch: None,
                switch_state: Arc::new(routing::ProviderSwitchState::default()),
                strip_model_prefix: None,
                add_model_prefix: None,
                fallback_on: ProxyConfig::default().fallback_on,
                request_body: serde_json::Map::new(),
            }],
            local_manager: None,
            voice: None,
        };

        let req: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "balanced",
            "messages": [{"role": "user", "content": "Hi"}]
        }))
        .unwrap();
        let response = chat_completions(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(
            provider_gateway.recorded_models(),
            vec!["qwen-test:small"],
            "provider gateway must receive the concrete constituent, not the synthetic alias"
        );
        assert!(
            default_gateway.recorded_models().is_empty(),
            "configured provider gateway should handle the concrete routed model"
        );
    }

    #[tokio::test]
    async fn provider_route_can_strip_public_model_prefix_before_upstream_request() {
        let default_gateway = Arc::new(RecordingGateway::new());
        let provider_gateway = Arc::new(RecordingGateway::new());
        let gateway: Arc<dyn GatewayBackend> = default_gateway.clone();
        let provider_gateway_dyn: Arc<dyn GatewayBackend> = provider_gateway.clone();
        let state = ProxyState {
            alloy_manager: Arc::new(AlloyManager::empty()),
            provider_registry: Arc::new(ProviderRegistry::new()),
            config: ProxyConfig {
                backend_type: "http".to_string(),
                ..Default::default()
            },
            model_shortcuts: Vec::new(),
            gateway,
            providers: vec![routing::ProviderEntry {
                id: "opencode-go".to_string(),
                patterns: vec!["opencode-go/kimi-k2.6".to_string()],
                gateway: provider_gateway_dyn,
                on_switch: None,
                switch_state: Arc::new(routing::ProviderSwitchState::default()),
                strip_model_prefix: Some("opencode-go/".to_string()),
                add_model_prefix: None,
                fallback_on: ProxyConfig::default().fallback_on,
                request_body: serde_json::Map::new(),
            }],
            local_manager: None,
            voice: None,
        };

        let req: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "opencode-go/kimi-k2.6",
            "messages": [{"role": "user", "content": "Hi"}]
        }))
        .unwrap();
        let response = chat_completions(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(
            provider_gateway.recorded_models(),
            vec!["kimi-k2.6"],
            "provider gateway should receive upstream model ID after namespace stripping"
        );
        assert!(
            default_gateway.recorded_models().is_empty(),
            "provider route should not fall through to default gateway"
        );
    }

    #[tokio::test]
    async fn provider_route_merges_configured_request_body_fields() {
        let default_gateway = Arc::new(RecordingGateway::new());
        let provider_gateway = Arc::new(RecordingGateway::new());
        let gateway: Arc<dyn GatewayBackend> = default_gateway.clone();
        let provider_gateway_dyn: Arc<dyn GatewayBackend> = provider_gateway.clone();
        let state = ProxyState {
            alloy_manager: Arc::new(AlloyManager::empty()),
            provider_registry: Arc::new(ProviderRegistry::new()),
            config: ProxyConfig {
                backend_type: "http".to_string(),
                ..Default::default()
            },
            model_shortcuts: Vec::new(),
            gateway,
            providers: vec![routing::ProviderEntry {
                id: "kimi-coding".to_string(),
                patterns: vec!["kimi-for-coding".to_string()],
                gateway: provider_gateway_dyn,
                on_switch: None,
                switch_state: Arc::new(routing::ProviderSwitchState::default()),
                strip_model_prefix: None,
                add_model_prefix: None,
                fallback_on: ProxyConfig::default().fallback_on,
                request_body: [(
                    "thinking".to_string(),
                    serde_json::json!({ "type": "disabled" }),
                )]
                .into_iter()
                .collect(),
            }],
            local_manager: None,
            voice: None,
        };

        let req: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "kimi-for-coding",
            "messages": [{"role": "user", "content": "Hi"}],
            "thinking": {"type": "enabled"}
        }))
        .unwrap();
        let response = chat_completions(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        let requests = provider_gateway.recorded_requests();
        assert_eq!(
            requests[0].extra_body.get("thinking"),
            Some(&serde_json::json!({ "type": "disabled" })),
            "provider request_body must be the operator-controlled override"
        );
    }

    #[tokio::test]
    async fn provider_route_runs_on_switch_before_gateway_request() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("switch.log");
        let script = format!(
            "printf 'hook stdout must stay out of service logs\\n'; printf 'hook stderr must stay out of service logs\\n' >&2; read ignored || true; printf '%s|%s|%s|%s\\n' \"$CALCIFORGE_PROVIDER_ID\" \"$CALCIFORGE_MODEL_ID\" \"$CALCIFORGE_UPSTREAM_MODEL_ID\" \"$CALCIFORGE_PREV_MODEL_ID\" >> '{}'",
            marker.display()
        );
        let default_gateway = Arc::new(RecordingGateway::new());
        let provider_gateway = Arc::new(RecordingGateway::new());
        let gateway: Arc<dyn GatewayBackend> = default_gateway.clone();
        let provider_gateway_dyn: Arc<dyn GatewayBackend> = provider_gateway.clone();
        let state = ProxyState {
            alloy_manager: Arc::new(AlloyManager::empty()),
            provider_registry: Arc::new(ProviderRegistry::new()),
            config: ProxyConfig {
                backend_type: "http".to_string(),
                ..Default::default()
            },
            model_shortcuts: Vec::new(),
            gateway,
            providers: vec![routing::ProviderEntry {
                id: "helicone-ollama".to_string(),
                patterns: vec!["local/qwen3.6:27b".to_string()],
                gateway: provider_gateway_dyn,
                on_switch: Some(script),
                switch_state: Arc::new(routing::ProviderSwitchState::default()),
                strip_model_prefix: Some("local/".to_string()),
                add_model_prefix: Some("ollama/".to_string()),
                fallback_on: ProxyConfig::default().fallback_on,
                request_body: serde_json::Map::new(),
            }],
            local_manager: None,
            voice: None,
        };

        let req: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "local/qwen3.6:27b",
            "messages": [{"role": "user", "content": "Hi"}]
        }))
        .unwrap();
        let response = chat_completions(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(
            provider_gateway.recorded_models(),
            vec!["ollama/qwen3.6:27b"]
        );
        assert_eq!(
            std::fs::read_to_string(marker).unwrap(),
            "helicone-ollama|local/qwen3.6:27b|ollama/qwen3.6:27b|\n"
        );
    }

    #[tokio::test]
    async fn provider_route_blocks_gateway_request_when_on_switch_fails() {
        let default_gateway = Arc::new(RecordingGateway::new());
        let provider_gateway = Arc::new(RecordingGateway::new());
        let gateway: Arc<dyn GatewayBackend> = default_gateway.clone();
        let provider_gateway_dyn: Arc<dyn GatewayBackend> = provider_gateway.clone();
        let state = ProxyState {
            alloy_manager: Arc::new(AlloyManager::empty()),
            provider_registry: Arc::new(ProviderRegistry::new()),
            config: ProxyConfig {
                backend_type: "http".to_string(),
                ..Default::default()
            },
            model_shortcuts: Vec::new(),
            gateway,
            providers: vec![routing::ProviderEntry {
                id: "helicone-ollama".to_string(),
                patterns: vec!["qwen3.6:27b".to_string()],
                gateway: provider_gateway_dyn,
                on_switch: Some("exit 42".to_string()),
                switch_state: Arc::new(routing::ProviderSwitchState::default()),
                strip_model_prefix: None,
                add_model_prefix: Some("ollama/".to_string()),
                fallback_on: ProxyConfig::default().fallback_on,
                request_body: serde_json::Map::new(),
            }],
            local_manager: None,
            voice: None,
        };

        let req: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "qwen3.6:27b",
            "messages": [{"role": "user", "content": "Hi"}]
        }))
        .unwrap();
        let response = chat_completions(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "unexpected body: {body}"
        );
        assert!(
            provider_gateway.recorded_models().is_empty(),
            "gateway must not receive a request when the model switch hook fails"
        );
    }

    #[tokio::test]
    async fn proxy_resolves_model_shortcuts_inside_dispatcher_constituents() {
        let alloy_manager = AlloyManager::from_gateway_configs(
            &[],
            &[],
            &[DispatcherConfig {
                id: "local-cloud-balanced".to_string(),
                name: Some("Local/cloud balanced".to_string()),
                models: vec![
                    SyntheticModelConfig {
                        model: "local".to_string(),
                        context_window: 60_000,
                    },
                    SyntheticModelConfig {
                        model: "cloud".to_string(),
                        context_window: 250_000,
                    },
                ],
            }],
        )
        .unwrap();

        let recording_gateway = Arc::new(RecordingGateway::new());
        let gateway: Arc<dyn GatewayBackend> = recording_gateway.clone();
        let state = ProxyState {
            alloy_manager: Arc::new(alloy_manager),
            provider_registry: Arc::new(ProviderRegistry::new()),
            config: ProxyConfig {
                backend_type: "http".to_string(),
                ..Default::default()
            },
            model_shortcuts: vec![
                ModelShortcutConfig {
                    alias: "local".to_string(),
                    model: "qwen-test:small".to_string(),
                },
                ModelShortcutConfig {
                    alias: "cloud".to_string(),
                    model: "kimi-test:medium".to_string(),
                },
            ],
            gateway,
            providers: Vec::new(),
            local_manager: None,
            voice: None,
        };

        let req: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "local-cloud-balanced",
            "messages": [{"role": "user", "content": "Hi"}]
        }))
        .unwrap();
        let response = chat_completions(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["model"], "qwen-test:small");
        assert_eq!(recording_gateway.recorded_models(), vec!["qwen-test:small"]);
    }

    #[tokio::test]
    async fn proxy_expands_constituent_shortcut_that_targets_another_synthetic_model() {
        let alloy_manager = AlloyManager::from_gateway_configs(
            &[],
            &[],
            &[
                DispatcherConfig {
                    id: "outer-dispatcher".to_string(),
                    name: Some("Outer dispatcher".to_string()),
                    models: vec![SyntheticModelConfig {
                        model: "inner".to_string(),
                        context_window: 250_000,
                    }],
                },
                DispatcherConfig {
                    id: "inner-dispatcher".to_string(),
                    name: Some("Inner dispatcher".to_string()),
                    models: vec![SyntheticModelConfig {
                        model: "qwen-test:small".to_string(),
                        context_window: 60_000,
                    }],
                },
            ],
        )
        .unwrap();

        let recording_gateway = Arc::new(RecordingGateway::new());
        let gateway: Arc<dyn GatewayBackend> = recording_gateway.clone();
        let state = ProxyState {
            alloy_manager: Arc::new(alloy_manager),
            provider_registry: Arc::new(ProviderRegistry::new()),
            config: ProxyConfig {
                backend_type: "http".to_string(),
                ..Default::default()
            },
            model_shortcuts: vec![ModelShortcutConfig {
                alias: "inner".to_string(),
                model: "inner-dispatcher".to_string(),
            }],
            gateway,
            providers: Vec::new(),
            local_manager: None,
            voice: None,
        };

        let req: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "outer-dispatcher",
            "messages": [{"role": "user", "content": "Hi"}]
        }))
        .unwrap();
        let response = chat_completions(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(body["model"], "qwen-test:small");
        assert_eq!(recording_gateway.recorded_models(), vec!["qwen-test:small"]);
    }

    #[tokio::test]
    async fn proxy_blocks_synthetic_plan_when_expanded_constituent_is_blocked() {
        let alloy_manager = AlloyManager::from_gateway_configs(
            &[],
            &[],
            &[DispatcherConfig {
                id: "local-cloud-balanced".to_string(),
                name: Some("Local/cloud balanced".to_string()),
                models: vec![
                    SyntheticModelConfig {
                        model: "local".to_string(),
                        context_window: 60_000,
                    },
                    SyntheticModelConfig {
                        model: "cloud".to_string(),
                        context_window: 250_000,
                    },
                ],
            }],
        )
        .unwrap();

        let recording_gateway = Arc::new(RecordingGateway::new());
        let gateway: Arc<dyn GatewayBackend> = recording_gateway.clone();
        let state = ProxyState {
            alloy_manager: Arc::new(alloy_manager),
            provider_registry: Arc::new(ProviderRegistry::new()),
            config: ProxyConfig {
                backend_type: "http".to_string(),
                default_policy: ProxyAccessPolicy::AllowConfigured,
                agents: vec![ProxyAgentConfig {
                    id: "test-agent".to_string(),
                    name: Some("Test Agent".to_string()),
                    api_key: None,
                    api_key_file: None,
                    allowed_models: vec!["*".to_string()],
                    blocked_models: vec!["qwen-test:small".to_string()],
                    rate_limit_rpm: 0,
                    rate_limit_tpm: 0,
                }],
                ..Default::default()
            },
            model_shortcuts: vec![
                ModelShortcutConfig {
                    alias: "local".to_string(),
                    model: "qwen-test:small".to_string(),
                },
                ModelShortcutConfig {
                    alias: "cloud".to_string(),
                    model: "kimi-test:medium".to_string(),
                },
            ],
            gateway,
            providers: Vec::new(),
            local_manager: None,
            voice: None,
        };

        let req: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "local-cloud-balanced",
            "messages": [{"role": "user", "content": "Hi"}]
        }))
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-agent-id", HeaderValue::from_static("test-agent"));
        let response = chat_completions(State(state), headers, Json(req))
            .await
            .into_response();

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status, StatusCode::FORBIDDEN, "unexpected body: {body}");
        assert_eq!(body["error"]["code"], "model_access_denied");
        assert!(recording_gateway.recorded_models().is_empty());
    }

    #[tokio::test]
    async fn proxy_allows_alias_to_synthetic_when_agent_allowed_root_model() {
        let alloy_manager = AlloyManager::from_gateway_configs(
            &[],
            &[],
            &[DispatcherConfig {
                id: "local-cloud-balanced".to_string(),
                name: Some("Local/cloud balanced".to_string()),
                models: vec![SyntheticModelConfig {
                    model: "local".to_string(),
                    context_window: 60_000,
                }],
            }],
        )
        .unwrap();

        let recording_gateway = Arc::new(RecordingGateway::new());
        let gateway: Arc<dyn GatewayBackend> = recording_gateway.clone();
        let state = ProxyState {
            alloy_manager: Arc::new(alloy_manager),
            provider_registry: Arc::new(ProviderRegistry::new()),
            config: ProxyConfig {
                backend_type: "http".to_string(),
                default_policy: ProxyAccessPolicy::AllowConfigured,
                agents: vec![ProxyAgentConfig {
                    id: "test-agent".to_string(),
                    name: Some("Test Agent".to_string()),
                    api_key: None,
                    api_key_file: None,
                    allowed_models: vec!["local-cloud-balanced".to_string()],
                    blocked_models: Vec::new(),
                    rate_limit_rpm: 0,
                    rate_limit_tpm: 0,
                }],
                ..Default::default()
            },
            model_shortcuts: vec![
                ModelShortcutConfig {
                    alias: "balanced".to_string(),
                    model: "local-cloud-balanced".to_string(),
                },
                ModelShortcutConfig {
                    alias: "local".to_string(),
                    model: "qwen-test:small".to_string(),
                },
            ],
            gateway,
            providers: Vec::new(),
            local_manager: None,
            voice: None,
        };

        let req: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "balanced",
            "messages": [{"role": "user", "content": "Hi"}]
        }))
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-agent-id", HeaderValue::from_static("test-agent"));
        let response = chat_completions(State(state), headers, Json(req))
            .await
            .into_response();

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert_eq!(recording_gateway.recorded_models(), vec!["qwen-test:small"]);
    }

    #[tokio::test]
    async fn proxy_rejects_alias_to_synthetic_cycle_as_invalid_model_plan() {
        let alloy_manager = AlloyManager::from_gateway_configs(
            &[],
            &[],
            &[DispatcherConfig {
                id: "local-cloud-balanced".to_string(),
                name: Some("Local/cloud balanced".to_string()),
                models: vec![SyntheticModelConfig {
                    model: "local".to_string(),
                    context_window: 60_000,
                }],
            }],
        )
        .unwrap();

        let recording_gateway = Arc::new(RecordingGateway::new());
        let gateway: Arc<dyn GatewayBackend> = recording_gateway.clone();
        let state = ProxyState {
            alloy_manager: Arc::new(alloy_manager),
            provider_registry: Arc::new(ProviderRegistry::new()),
            config: ProxyConfig {
                backend_type: "http".to_string(),
                ..Default::default()
            },
            model_shortcuts: vec![ModelShortcutConfig {
                alias: "local".to_string(),
                model: "local-cloud-balanced".to_string(),
            }],
            gateway,
            providers: Vec::new(),
            local_manager: None,
            voice: None,
        };

        let req: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "local-cloud-balanced",
            "messages": [{"role": "user", "content": "Hi"}]
        }))
        .unwrap();
        let response = chat_completions(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status, StatusCode::BAD_REQUEST, "unexpected body: {body}");
        assert_eq!(body["error"]["code"], "invalid_model_plan");
        assert!(recording_gateway.recorded_models().is_empty());
    }

    #[tokio::test]
    async fn list_models_includes_model_shortcut_aliases() {
        let recording_gateway = Arc::new(RecordingGateway::new());
        let gateway: Arc<dyn GatewayBackend> = recording_gateway;
        let state = ProxyState {
            alloy_manager: Arc::new(AlloyManager::empty()),
            provider_registry: Arc::new(ProviderRegistry::new()),
            config: ProxyConfig {
                backend_type: "http".to_string(),
                ..Default::default()
            },
            model_shortcuts: vec![ModelShortcutConfig {
                alias: "local".to_string(),
                model: "qwen-test:small".to_string(),
            }],
            gateway,
            providers: Vec::new(),
            local_manager: None,
            voice: None,
        };

        let response = list_models(State(state), HeaderMap::new())
            .await
            .into_response();

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        assert!(
            body["data"].as_array().unwrap().iter().any(|model| {
                model["id"] == "local"
                    && model["owned_by"]
                        .as_str()
                        .is_some_and(|owner| owner.contains("qwen-test:small"))
            }),
            "shortcut alias should be advertised as a valid model: {body}"
        );
    }

    #[tokio::test]
    async fn list_models_includes_exact_configured_provider_models() {
        let recording_gateway = Arc::new(RecordingGateway::new());
        let gateway: Arc<dyn GatewayBackend> = recording_gateway.clone();
        let provider_gateway: Arc<dyn GatewayBackend> = recording_gateway;
        let state = ProxyState {
            alloy_manager: Arc::new(AlloyManager::empty()),
            provider_registry: Arc::new(ProviderRegistry::new()),
            config: ProxyConfig {
                backend_type: "http".to_string(),
                ..Default::default()
            },
            model_shortcuts: Vec::new(),
            gateway,
            providers: vec![routing::ProviderEntry {
                id: "subscription".to_string(),
                patterns: vec![
                    "gpt-5.5".to_string(),
                    "codex/*".to_string(),
                    "*".to_string(),
                ],
                gateway: provider_gateway,
                on_switch: None,
                switch_state: Arc::new(routing::ProviderSwitchState::default()),
                strip_model_prefix: None,
                add_model_prefix: None,
                fallback_on: ProxyConfig::default().fallback_on,
                request_body: serde_json::Map::new(),
            }],
            local_manager: None,
            voice: None,
        };

        let response = list_models(State(state), HeaderMap::new())
            .await
            .into_response();

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
        let model_ids: Vec<&str> = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|model| model["id"].as_str())
            .collect();
        assert!(
            model_ids.contains(&"gpt-5.5"),
            "exact configured provider model should be advertised: {body}"
        );
        assert!(
            !model_ids.contains(&"codex/*") && !model_ids.contains(&"*"),
            "wildcard provider patterns are route patterns, not selectable model IDs: {body}"
        );
    }
}
