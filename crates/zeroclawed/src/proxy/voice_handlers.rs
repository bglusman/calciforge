//! Handlers for voice passthrough and tool manifest endpoints.

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use tracing::{info, warn};

use crate::voice::{forward, tools};
use super::ProxyState;

/// POST /v1/audio/transcriptions
///
/// Forwards the raw multipart request body to the configured STT endpoint,
/// optionally passing it through the `on_audio_in` hook first.
/// Returns 501 if no STT endpoint is configured.
pub async fn audio_transcriptions(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(ref voice) = state.voice else {
        return not_configured("stt");
    };
    let Some(ref stt) = voice.stt else {
        return not_configured("stt");
    };

    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("multipart/form-data");

    // Run optional pre-processing hook.
    let body = forward::run_hook(voice.hooks.on_audio_in.as_deref(), body).await;

    info!(bytes = body.len(), url = %stt.url, "forwarding STT request");

    match forward::forward_stt(stt, body, content_type).await {
        Ok((resp_body, resp_ct)) => (
            [(axum::http::header::CONTENT_TYPE, resp_ct)],
            resp_body,
        )
            .into_response(),
        Err(e) => {
            warn!(error = %e, "STT forward failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response()
        }
    }
}

/// POST /v1/audio/speech
///
/// Forwards the JSON synthesis request to the configured TTS endpoint,
/// optionally passing the text through the `on_text_out` hook first.
/// Returns 501 if no TTS endpoint is configured.
pub async fn audio_speech(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(ref voice) = state.voice else {
        return not_configured("tts");
    };
    let Some(ref tts) = voice.tts else {
        return not_configured("tts");
    };

    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json");

    // Run optional text post-processing hook (operates on the JSON body).
    // Hook receives raw JSON bytes; for text manipulation the hook can parse
    // and rewrite the "input" field, or pass through unchanged.
    let body = forward::run_hook(voice.hooks.on_text_out.as_deref(), body).await;

    info!(bytes = body.len(), url = %tts.url, "forwarding TTS request");

    match forward::forward_tts(tts, body, content_type).await {
        Ok((resp_body, resp_ct)) => (
            [(axum::http::header::CONTENT_TYPE, resp_ct)],
            resp_body,
        )
            .into_response(),
        Err(e) => {
            warn!(error = %e, "TTS forward failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response()
        }
    }
}

/// GET /v1/tools/manifest
///
/// Returns zeroclawed's own capabilities as an array of OpenAI-compatible
/// tool definitions. The list reflects what is actually configured on this
/// instance — a model can inject these directly into its `tools` parameter.
pub async fn tools_manifest(State(state): State<ProxyState>) -> impl IntoResponse {
    let manifest = tools::build_manifest(
        state.local_manager.as_deref(),
        state.voice.as_ref().and_then(|v| v.stt.as_ref()).is_some(),
        state.voice.as_ref().and_then(|v| v.tts.as_ref()).is_some(),
    );
    Json(manifest)
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn not_configured(endpoint: &str) -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": format!(
                "No {endpoint} endpoint configured. Add [proxy.voice.{endpoint}] to your config."
            )
        })),
    )
        .into_response()
}
