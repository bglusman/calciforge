//! Voice pipeline surface for calciforge.
//!
//! Calciforge does not own a voice pipeline. It provides three thin integration
//! points that external pipelines (or the model itself) can call into:
//!
//! 1. **STT passthrough** — `POST /v1/audio/transcriptions`
//!    Forwards multipart audio to whatever STT server is configured.
//!
//! 2. **TTS passthrough** — `POST /v1/audio/speech`
//!    Forwards synthesis requests to whatever TTS server is configured.
//!
//! 3. **Tool manifest** — `GET /v1/tools/manifest`
//!    Returns calciforge's own capabilities as OpenAI-compatible tool definitions
//!    so any model can call into calciforge (switch model, list models, etc.)
//!    without hardcoded knowledge of this instance.
//!
//! All three endpoints are optional. STT/TTS return 501 when not configured.
//! The tool manifest is always available (it reflects what *is* configured).

pub mod forward;
pub mod tools;

use serde::{Deserialize, Serialize};

/// Top-level voice configuration (`[proxy.voice]` in TOML).
///
/// All fields are optional — omit the whole section to disable voice endpoints.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct VoiceConfig {
    /// STT (speech-to-text) endpoint.
    /// Must speak OpenAI-compatible `POST /audio/transcriptions` (multipart/form-data).
    #[serde(default)]
    pub stt: Option<VoiceEndpointConfig>,

    /// TTS (text-to-speech) endpoint.
    /// Must speak OpenAI-compatible `POST /audio/speech` (JSON → audio bytes).
    #[serde(default)]
    pub tts: Option<VoiceEndpointConfig>,

    /// Optional shell hooks. Each hook receives input on stdin and returns
    /// transformed output on stdout. Stderr is logged and ignored.
    #[serde(default)]
    pub hooks: VoiceHooksConfig,
}

/// Configuration for a single voice passthrough endpoint.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VoiceEndpointConfig {
    /// Full base URL of the upstream service, e.g. `http://localhost:8178`.
    /// The appropriate path (`/v1/audio/transcriptions` etc.) is appended.
    pub url: String,

    /// Optional bearer token for the upstream service.
    #[serde(default)]
    pub api_key: Option<String>,

    /// Request timeout in seconds. Default: 60.
    #[serde(default = "default_voice_timeout")]
    pub timeout_seconds: u64,
}

fn default_voice_timeout() -> u64 {
    60
}

/// Optional shell hooks for audio pre/post processing.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct VoiceHooksConfig {
    /// Path to script invoked before audio is sent to STT.
    /// Receives raw audio bytes on stdin; must write (possibly transformed)
    /// audio bytes to stdout. Use for VAD, normalization, speaker filtering, etc.
    #[serde(default)]
    pub on_audio_in: Option<String>,

    /// Path to script invoked on the text response before it is sent to TTS.
    /// Receives UTF-8 text on stdin; must write text to stdout.
    /// Use for filtering, annotation, ssml injection, etc.
    #[serde(default)]
    pub on_text_out: Option<String>,
}
