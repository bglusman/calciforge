use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Observability sink for model-gateway attempt telemetry.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProxyObservabilityConfig {
    /// Sink type. Supported values: "log", "http-json" (alias "webhook"),
    /// "otel" (alias "otlp"), and "traceloop"; underscores normalize to
    /// hyphens.
    #[serde(default = "default_observability_kind")]
    pub kind: String,
    /// Whether this sink is active.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Destination endpoint for network sinks. For OTLP sinks this may be a
    /// collector base URL or a full `/v1/traces` endpoint.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Extra headers for network sinks. Values must be operator-controlled and
    /// are never copied into telemetry payloads.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Per-sink timeout in milliseconds.
    #[serde(default = "default_observability_timeout_ms")]
    pub timeout_ms: u64,
}

impl Default for ProxyObservabilityConfig {
    fn default() -> Self {
        Self {
            kind: default_observability_kind(),
            enabled: default_true(),
            endpoint: None,
            headers: HashMap::new(),
            timeout_ms: default_observability_timeout_ms(),
        }
    }
}

fn default_observability_kind() -> String {
    "log".to_string()
}

fn default_true() -> bool {
    true
}

fn default_observability_timeout_ms() -> u64 {
    250
}
