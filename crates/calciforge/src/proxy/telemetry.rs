//! Shared model-gateway telemetry sinks.
//!
//! This layer observes routing attempts after Calciforge has selected a
//! concrete provider/model. It deliberately avoids prompts, completions,
//! headers, and query strings so observability does not become a secret sink.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Serialize;
use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::config::{GatewayFailureKind, ProxyObservabilityConfig};
use crate::proxy::openai::ChatCompletionResponse;
use crate::sync::Arc;

pub(crate) const SUPPORTED_OBSERVABILITY_KINDS: &[&str] =
    &["log", "http-json", "webhook", "otel", "otlp", "traceloop"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GatewayTelemetryOutcome {
    Success,
    Failure,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GatewayTelemetryEvent {
    pub event_type: &'static str,
    pub timestamp_ms: u64,
    pub agent_id: String,
    pub requested_model: String,
    pub root_model: String,
    pub concrete_model: String,
    pub upstream_model: String,
    pub provider_id: Option<String>,
    pub gateway_engine: String,
    pub stream: bool,
    pub tools: bool,
    pub message_count: usize,
    pub duration_ms: u64,
    pub outcome: GatewayTelemetryOutcome,
    pub failure_kind: Option<GatewayFailureKind>,
    pub choices: Option<usize>,
    pub receipt_id: Option<String>,
}

pub(crate) struct GatewayTelemetryAttempt {
    pub agent_id: String,
    pub requested_model: String,
    pub root_model: String,
    pub concrete_model: String,
    pub upstream_model: String,
    pub provider_id: Option<String>,
    pub gateway_engine: String,
    pub stream: bool,
    pub tools: bool,
    pub message_count: usize,
}

impl GatewayTelemetryAttempt {
    pub(crate) fn success_with_receipt_id(
        self,
        duration: Duration,
        choices: usize,
        receipt_id: Option<&str>,
    ) -> GatewayTelemetryEvent {
        GatewayTelemetryEvent {
            event_type: "model_gateway.attempt",
            timestamp_ms: timestamp_ms(),
            agent_id: self.agent_id,
            requested_model: self.requested_model,
            root_model: self.root_model,
            concrete_model: self.concrete_model,
            upstream_model: self.upstream_model,
            provider_id: self.provider_id,
            gateway_engine: self.gateway_engine,
            stream: self.stream,
            tools: self.tools,
            message_count: self.message_count,
            duration_ms: duration.as_millis() as u64,
            outcome: GatewayTelemetryOutcome::Success,
            failure_kind: None,
            choices: Some(choices),
            receipt_id: receipt_id.map(str::to_string),
        }
    }

    pub(crate) fn success_response(
        self,
        duration: Duration,
        response: &ChatCompletionResponse,
    ) -> GatewayTelemetryEvent {
        self.success_with_receipt_id(
            duration,
            response.choices.len(),
            response.wardwright_receipt_id(),
        )
    }

    pub(crate) fn failure(
        self,
        duration: Duration,
        failure_kind: GatewayFailureKind,
    ) -> GatewayTelemetryEvent {
        GatewayTelemetryEvent {
            event_type: "model_gateway.attempt",
            timestamp_ms: timestamp_ms(),
            agent_id: self.agent_id,
            requested_model: self.requested_model,
            root_model: self.root_model,
            concrete_model: self.concrete_model,
            upstream_model: self.upstream_model,
            provider_id: self.provider_id,
            gateway_engine: self.gateway_engine,
            stream: self.stream,
            tools: self.tools,
            message_count: self.message_count,
            duration_ms: duration.as_millis() as u64,
            outcome: GatewayTelemetryOutcome::Failure,
            failure_kind: Some(failure_kind),
            choices: None,
            receipt_id: None,
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct TelemetryFanout {
    sinks: Arc<Vec<Arc<dyn TelemetrySink>>>,
}

impl TelemetryFanout {
    pub(crate) fn from_config(configs: &[ProxyObservabilityConfig]) -> anyhow::Result<Self> {
        let mut sinks: Vec<Arc<dyn TelemetrySink>> = Vec::new();

        for config in configs.iter().filter(|config| config.enabled) {
            let kind = normalize_kind(&config.kind);
            match kind.as_str() {
                "log" => sinks.push(Arc::new(LogTelemetrySink)),
                "http-json" | "webhook" => {
                    sinks.push(Arc::new(HttpJsonTelemetrySink::new(config, false)?));
                }
                "otel" | "otlp" | "traceloop" => {
                    sinks.push(Arc::new(HttpJsonTelemetrySink::new(config, true)?));
                }
                _ => anyhow::bail!(
                    "unsupported proxy observability kind '{}'. Supported values: {}",
                    config.kind,
                    SUPPORTED_OBSERVABILITY_KINDS.join(", ")
                ),
            }
        }

        Ok(Self {
            sinks: Arc::new(sinks),
        })
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.sinks.is_empty()
    }

    pub(crate) async fn emit_gateway_attempt(&self, event: GatewayTelemetryEvent) {
        for sink in self.sinks.iter() {
            let sink = Arc::clone(sink);
            let event = event.clone();
            tokio::task::spawn(async move {
                if let Err(_err) = sink.emit_gateway_attempt(&event).await {
                    warn!(sink = sink.name(), "Gateway telemetry sink failed");
                }
            });
        }
    }
}

#[async_trait]
trait TelemetrySink: Send + Sync {
    fn name(&self) -> &'static str;
    async fn emit_gateway_attempt(&self, event: &GatewayTelemetryEvent) -> anyhow::Result<()>;
}

struct LogTelemetrySink;

#[async_trait]
impl TelemetrySink for LogTelemetrySink {
    fn name(&self) -> &'static str {
        "log"
    }

    async fn emit_gateway_attempt(&self, event: &GatewayTelemetryEvent) -> anyhow::Result<()> {
        match event.outcome {
            GatewayTelemetryOutcome::Success => debug!(
                agent_id = %event.agent_id,
                requested_model = %event.requested_model,
                root_model = %event.root_model,
                concrete_model = %event.concrete_model,
                upstream_model = %event.upstream_model,
                provider_id = ?event.provider_id,
                gateway_engine = %event.gateway_engine,
                duration_ms = event.duration_ms,
                choices = event.choices.unwrap_or_default(),
                receipt_id = ?event.receipt_id,
                "Model gateway attempt succeeded"
            ),
            GatewayTelemetryOutcome::Failure => warn!(
                agent_id = %event.agent_id,
                requested_model = %event.requested_model,
                root_model = %event.root_model,
                concrete_model = %event.concrete_model,
                upstream_model = %event.upstream_model,
                provider_id = ?event.provider_id,
                gateway_engine = %event.gateway_engine,
                duration_ms = event.duration_ms,
                failure_kind = ?event.failure_kind,
                "Model gateway attempt failed"
            ),
        }
        Ok(())
    }
}

struct HttpJsonTelemetrySink {
    client: reqwest::Client,
    endpoint: String,
    headers: HeaderMap,
    otlp: bool,
}

impl HttpJsonTelemetrySink {
    fn new(config: &ProxyObservabilityConfig, otlp: bool) -> anyhow::Result<Self> {
        let Some(endpoint) = config
            .endpoint
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            anyhow::bail!(
                "proxy observability kind '{}' requires endpoint",
                config.kind
            );
        };
        let endpoint = if otlp {
            otlp_traces_endpoint(endpoint)
        } else {
            endpoint.to_string()
        };

        let mut headers = HeaderMap::new();
        for (name, value) in &config.headers {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes())?,
                HeaderValue::from_str(value)?,
            );
        }

        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()?;

        Ok(Self {
            client,
            endpoint,
            headers,
            otlp,
        })
    }
}

#[async_trait]
impl TelemetrySink for HttpJsonTelemetrySink {
    fn name(&self) -> &'static str {
        if self.otlp { "otlp-json" } else { "http-json" }
    }

    async fn emit_gateway_attempt(&self, event: &GatewayTelemetryEvent) -> anyhow::Result<()> {
        let body = if self.otlp {
            otlp_trace_export(event)
        } else {
            serde_json::to_value(event)?
        };

        let response = self
            .client
            .post(&self.endpoint)
            .headers(self.headers.clone())
            .json(&body)
            .send()
            .await
            .map_err(|err| anyhow::anyhow!("{}", safe_reqwest_error(&err)))?;

        if !response.status().is_success() {
            anyhow::bail!("telemetry endpoint returned HTTP {}", response.status());
        }
        Ok(())
    }
}

fn normalize_kind(kind: &str) -> String {
    kind.trim().to_ascii_lowercase().replace('_', "-")
}

fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn otlp_traces_endpoint(endpoint: &str) -> String {
    let trimmed = endpoint.trim().trim_end_matches('/');
    if trimmed.ends_with("/v1/traces") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1/traces")
    }
}

fn otlp_trace_export(event: &GatewayTelemetryEvent) -> Value {
    let trace_id = uuid::Uuid::new_v4().simple().to_string();
    let span_id = uuid::Uuid::new_v4().simple().to_string()[..16].to_string();
    let end_ns = event.timestamp_ms.saturating_mul(1_000_000);
    let start_ns = end_ns.saturating_sub(event.duration_ms.saturating_mul(1_000_000));
    let status_code = match event.outcome {
        GatewayTelemetryOutcome::Success => 1,
        GatewayTelemetryOutcome::Failure => 2,
    };

    json!({
        "resourceSpans": [{
            "resource": {
                "attributes": [
                    otlp_attr("service.name", "calciforge"),
                    otlp_attr("service.version", env!("CARGO_PKG_VERSION"))
                ]
            },
            "scopeSpans": [{
                "scope": {
                    "name": "calciforge.proxy.telemetry",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "spans": [{
                    "traceId": trace_id,
                    "spanId": span_id,
                    "name": event.event_type,
                    "kind": 3,
                    "startTimeUnixNano": start_ns.to_string(),
                    "endTimeUnixNano": end_ns.to_string(),
                    "attributes": otlp_attributes(event),
                    "status": { "code": status_code }
                }]
            }]
        }]
    })
}

fn otlp_attributes(event: &GatewayTelemetryEvent) -> Vec<Value> {
    let mut attrs = vec![
        otlp_attr("gen_ai.operation.name", "chat"),
        otlp_attr("gen_ai.request.model", &event.requested_model),
        otlp_attr("gen_ai.response.model", &event.upstream_model),
        otlp_attr("calciforge.agent_id", &event.agent_id),
        otlp_attr("calciforge.root_model", &event.root_model),
        otlp_attr("calciforge.concrete_model", &event.concrete_model),
        otlp_attr("calciforge.gateway_engine", &event.gateway_engine),
        otlp_bool_attr("calciforge.stream", event.stream),
        otlp_bool_attr("calciforge.tools", event.tools),
        otlp_i64_attr("calciforge.message_count", event.message_count as i64),
        otlp_i64_attr("calciforge.duration_ms", event.duration_ms as i64),
        otlp_attr(
            "calciforge.outcome",
            match event.outcome {
                GatewayTelemetryOutcome::Success => "success",
                GatewayTelemetryOutcome::Failure => "failure",
            },
        ),
    ];
    if let Some(provider_id) = event.provider_id.as_deref() {
        attrs.push(otlp_attr("calciforge.provider_id", provider_id));
    }
    if let Some(failure_kind) = event.failure_kind {
        attrs.push(otlp_attr(
            "calciforge.failure_kind",
            &format!("{failure_kind:?}"),
        ));
    }
    if let Some(choices) = event.choices {
        attrs.push(otlp_i64_attr("calciforge.choices", choices as i64));
    }
    if let Some(receipt_id) = event.receipt_id.as_deref() {
        attrs.push(otlp_attr("calciforge.receipt_id", receipt_id));
    }
    attrs
}

fn otlp_attr(key: &str, value: &str) -> Value {
    json!({ "key": key, "value": { "stringValue": value } })
}

fn otlp_bool_attr(key: &str, value: bool) -> Value {
    json!({ "key": key, "value": { "boolValue": value } })
}

fn otlp_i64_attr(key: &str, value: i64) -> Value {
    json!({ "key": key, "value": { "intValue": value.to_string() } })
}

fn safe_reqwest_error(err: &reqwest::Error) -> String {
    let mut parts = Vec::new();
    if err.is_timeout() {
        parts.push("timeout".to_string());
    }
    if err.is_connect() {
        parts.push("connect".to_string());
    }
    if err.is_request() {
        parts.push("request".to_string());
    }
    if let Some(status) = err.status() {
        parts.push(format!("status={status}"));
    }
    if parts.is_empty() {
        "telemetry request failed".to_string()
    } else {
        format!("telemetry request failed ({})", parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    use mockito::Matcher;

    use super::*;

    #[derive(Clone)]
    struct SlowSink {
        completed: Arc<AtomicBool>,
    }

    #[async_trait]
    impl TelemetrySink for SlowSink {
        fn name(&self) -> &'static str {
            "slow-test"
        }

        async fn emit_gateway_attempt(&self, _event: &GatewayTelemetryEvent) -> anyhow::Result<()> {
            tokio::time::sleep(Duration::from_millis(200)).await;
            self.completed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    fn fanout_from_test_sinks(sinks: Vec<Arc<dyn TelemetrySink>>) -> TelemetryFanout {
        TelemetryFanout {
            sinks: Arc::new(sinks),
        }
    }

    fn event() -> GatewayTelemetryEvent {
        GatewayTelemetryAttempt {
            agent_id: "agent-1".to_string(),
            requested_model: "smart".to_string(),
            root_model: "smart".to_string(),
            concrete_model: "qwen".to_string(),
            upstream_model: "qwen3.6".to_string(),
            provider_id: Some("local".to_string()),
            gateway_engine: "litellm".to_string(),
            stream: false,
            tools: true,
            message_count: 2,
        }
        .success_with_receipt_id(Duration::from_millis(42), 1, None)
    }

    #[test]
    fn disabled_sinks_are_not_registered() {
        let fanout = TelemetryFanout::from_config(&[ProxyObservabilityConfig {
            enabled: false,
            ..ProxyObservabilityConfig::default()
        }])
        .expect("fanout");

        assert!(fanout.is_empty());
    }

    #[tokio::test]
    async fn http_json_sink_posts_redacted_event_metadata() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/events")
            .match_body(Matcher::PartialJson(json!({
                "event_type": "model_gateway.attempt",
                "agent_id": "agent-1",
                "requested_model": "smart",
                "concrete_model": "qwen",
                "upstream_model": "qwen3.6",
                "provider_id": "local"
            })))
            .with_status(204)
            .create_async()
            .await;

        let fanout = TelemetryFanout::from_config(&[ProxyObservabilityConfig {
            kind: "http-json".to_string(),
            endpoint: Some(format!("{}/events", server.url())),
            ..ProxyObservabilityConfig::default()
        }])
        .expect("fanout");

        fanout.emit_gateway_attempt(event()).await;

        wait_for_mock(&mock).await;
    }

    #[tokio::test]
    async fn traceloop_sink_uses_otlp_traces_endpoint() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/traces")
            .match_body(Matcher::PartialJson(json!({
                "resourceSpans": [{
                    "scopeSpans": [{
                        "spans": [{
                            "name": "model_gateway.attempt",
                            "kind": 3
                        }]
                    }]
                }]
            })))
            .with_status(200)
            .create_async()
            .await;

        let fanout = TelemetryFanout::from_config(&[ProxyObservabilityConfig {
            kind: "traceloop".to_string(),
            endpoint: Some(server.url()),
            ..ProxyObservabilityConfig::default()
        }])
        .expect("fanout");

        fanout.emit_gateway_attempt(event()).await;

        wait_for_mock(&mock).await;
    }

    #[tokio::test]
    async fn fanout_does_not_block_request_path_on_slow_sink() {
        let completed = Arc::new(AtomicBool::new(false));
        let fanout = fanout_from_test_sinks(vec![Arc::new(SlowSink {
            completed: completed.clone(),
        })]);

        let started = Instant::now();
        fanout.emit_gateway_attempt(event()).await;

        assert!(
            started.elapsed() < Duration::from_millis(50),
            "telemetry fanout should return before slow sinks finish"
        );
        assert!(
            !completed.load(Ordering::SeqCst),
            "slow sink should still be running after fire-and-forget emit"
        );

        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(
            completed.load(Ordering::SeqCst),
            "slow sink should still be executed in the background"
        );
    }

    #[test]
    fn otlp_span_ends_at_event_timestamp_and_starts_before_duration() {
        let mut event = event();
        event.timestamp_ms = 1_000;
        event.duration_ms = 42;

        let export = otlp_trace_export(&event);
        let span = &export["resourceSpans"][0]["scopeSpans"][0]["spans"][0];

        assert_eq!(span["endTimeUnixNano"], "1000000000");
        assert_eq!(span["startTimeUnixNano"], "958000000");
    }

    async fn wait_for_mock(mock_: &mockito::Mock) {
        for _ in 0..20 {
            if mock_.matched_async().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        mock_.assert_async().await;
    }
}
