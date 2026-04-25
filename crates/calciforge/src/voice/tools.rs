//! Tool manifest — calciforge's own capabilities as OpenAI tool definitions.
//!
//! `GET /v1/tools/manifest` returns a JSON array of tool definitions that any
//! model can inject into its `tools` parameter to call back into calciforge.
//!
//! Only capabilities that are actually configured/enabled appear in the manifest.
//! A model querying this endpoint gets an accurate picture of what it can do
//! on this specific calciforge instance without hardcoding assumptions.

use serde_json::{json, Value};

use crate::local_model::LocalModelManager;

/// Build the tool manifest for this calciforge instance.
///
/// `local_manager` is Some when `[local_models]` is configured.
/// `has_stt` / `has_tts` reflect whether voice endpoints are wired.
pub fn build_manifest(
    local_manager: Option<&LocalModelManager>,
    has_stt: bool,
    has_tts: bool,
) -> Vec<Value> {
    let mut tools: Vec<Value> = Vec::new();

    // ── Local model switching ───────────────────────────────────────────────
    if let Some(mgr) = local_manager {
        let model_ids: Vec<&str> = mgr.models().iter().map(|m| m.id.as_str()).collect();

        tools.push(json!({
            "type": "function",
            "function": {
                "name": "calciforge_switch_model",
                "description": format!(
                    "Switch the currently loaded local inference model. \
                     Available models: {}.",
                    model_ids.join(", ")
                ),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "model": {
                            "type": "string",
                            "description": "Short model ID to load (e.g. \"qwen3-8bit\")",
                            "enum": model_ids
                        }
                    },
                    "required": ["model"]
                }
            }
        }));

        if let Some(current) = mgr.current() {
            tools.push(json!({
                "type": "function",
                "function": {
                    "name": "calciforge_current_model",
                    "description": format!(
                        "Returns the currently loaded local model. Currently: {}.",
                        current.id
                    ),
                    "parameters": {
                        "type": "object",
                        "properties": {}
                    }
                }
            }));
        }
    }

    // ── Voice passthrough availability ─────────────────────────────────────
    if has_stt {
        tools.push(json!({
            "type": "function",
            "function": {
                "name": "calciforge_transcribe",
                "description": "Transcribe audio to text via the configured STT endpoint \
                                (POST /v1/audio/transcriptions). Accepts multipart/form-data \
                                with an 'audio' field.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "audio_url": {
                            "type": "string",
                            "description": "URL of audio file accessible to calciforge"
                        },
                        "language": {
                            "type": "string",
                            "description": "BCP-47 language code hint (optional)"
                        }
                    },
                    "required": ["audio_url"]
                }
            }
        }));
    }

    if has_tts {
        tools.push(json!({
            "type": "function",
            "function": {
                "name": "calciforge_speak",
                "description": "Synthesize text to speech via the configured TTS endpoint \
                                (POST /v1/audio/speech). Returns audio.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "text": {
                            "type": "string",
                            "description": "Text to synthesize"
                        },
                        "voice": {
                            "type": "string",
                            "description": "Voice name/id (passed to upstream TTS; optional)"
                        }
                    },
                    "required": ["text"]
                }
            }
        }));
    }

    tools
}
