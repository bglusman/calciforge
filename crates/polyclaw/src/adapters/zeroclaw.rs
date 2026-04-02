//! ZeroClawAdapter — dispatches to ZeroClaw's custom webhook protocol.
//!
//! ZeroClaw (Rust binary on CT 1200, port 18792) speaks a simple custom protocol:
//!
//! ```text
//! POST {endpoint}/webhook
//! Authorization: Bearer {api_key}
//! Content-Type: application/json
//!
//! {"message": "text"}
//!
//! → {"model": "kimi-k2.5", "response": "text"}
//! ```
//!
//! This is explicitly NOT OpenAI-compat — the request and response shapes differ.

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::{AdapterError, AgentAdapter};

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// ZeroClaw webhook request body.
#[derive(Debug, Serialize)]
struct WebhookRequest {
    message: String,
}

/// ZeroClaw webhook response body.
#[derive(Debug, Deserialize)]
struct WebhookResponse {
    /// The model that responded (e.g. "kimi-k2.5"). Logged but not returned to caller.
    #[allow(dead_code)]
    model: Option<String>,
    /// The response text.
    response: String,
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

const DEFAULT_TIMEOUT_MS: u64 = 90_000;

/// Adapter for ZeroClaw's custom webhook protocol.
pub struct ZeroClawAdapter {
    client: reqwest::Client,
    endpoint: String,
    api_key: String,
    timeout: Duration,
}

impl ZeroClawAdapter {
    /// Create a new ZeroClaw adapter.
    ///
    /// - `endpoint` — base URL, e.g. `http://127.0.0.1:18792`
    /// - `api_key` — Bearer token (per-agent, required)
    /// - `timeout_ms` — per-request timeout (`None` → 90 000 ms)
    pub fn new(endpoint: String, api_key: String, timeout_ms: Option<u64>) -> Self {
        let timeout = Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .expect("reqwest client");
        Self {
            client,
            endpoint,
            api_key,
            timeout,
        }
    }
}

#[async_trait]
impl AgentAdapter for ZeroClawAdapter {
    async fn dispatch(&self, msg: &str) -> Result<String, AdapterError> {
        let url = format!("{}/webhook", self.endpoint.trim_end_matches('/'));

        let body = WebhookRequest {
            message: msg.to_string(),
        };

        info!(endpoint = %url, "zeroclaw dispatch");
        debug!(msg = %msg, "outbound message");

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .timeout(self.timeout)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    AdapterError::Timeout
                } else {
                    AdapterError::Unavailable(e.to_string())
                }
            })?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            warn!(status = %status, body = %body_text, "zeroclaw returned error status");
            return Err(AdapterError::Protocol(format!(
                "HTTP {}: {}",
                status, body_text
            )));
        }

        let webhook_resp: WebhookResponse = resp.json().await.map_err(|e| {
            AdapterError::Protocol(format!("failed to parse zeroclaw response: {}", e))
        })?;

        if let Some(ref model) = webhook_resp.model {
            info!(model = %model, "zeroclaw: received response");
        } else {
            info!("zeroclaw: received response");
        }
        debug!(response = %webhook_resp.response, "zeroclaw response");

        Ok(webhook_resp.response)
    }

    fn kind(&self) -> &'static str {
        "zeroclaw"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_adapter(port: u16) -> ZeroClawAdapter {
        ZeroClawAdapter::new(
            format!("http://127.0.0.1:{}", port),
            "zc_test_key".to_string(),
            Some(2000),
        )
    }

    #[test]
    fn test_kind_is_zeroclaw() {
        let adapter = make_adapter(19002);
        assert_eq!(adapter.kind(), "zeroclaw");
    }

    #[test]
    fn test_webhook_url_construction() {
        let endpoint = "http://127.0.0.1:18792";
        let url = format!("{}/webhook", endpoint.trim_end_matches('/'));
        assert_eq!(url, "http://127.0.0.1:18792/webhook");
    }

    #[test]
    fn test_webhook_url_construction_trailing_slash() {
        let endpoint = "http://127.0.0.1:18792/";
        let url = format!("{}/webhook", endpoint.trim_end_matches('/'));
        assert_eq!(url, "http://127.0.0.1:18792/webhook");
    }

    #[test]
    fn test_webhook_request_serialization() {
        let req = WebhookRequest {
            message: "hello zeroclaw".to_string(),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["message"], "hello zeroclaw");
        // Should NOT have "model" or "messages" keys
        assert!(json.get("model").is_none());
        assert!(json.get("messages").is_none());
    }

    #[test]
    fn test_webhook_response_deserialization_with_model() {
        let raw = r#"{"model": "kimi-k2.5", "response": "hello back"}"#;
        let resp: WebhookResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.response, "hello back");
        assert_eq!(resp.model.as_deref(), Some("kimi-k2.5"));
    }

    #[test]
    fn test_webhook_response_deserialization_no_model() {
        let raw = r#"{"response": "pong"}"#;
        let resp: WebhookResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.response, "pong");
        assert!(resp.model.is_none());
    }

    #[test]
    fn test_default_timeout() {
        let adapter = ZeroClawAdapter::new("http://localhost".to_string(), "key".to_string(), None);
        assert_eq!(adapter.timeout, Duration::from_millis(DEFAULT_TIMEOUT_MS));
    }

    #[test]
    fn test_custom_timeout() {
        let adapter = ZeroClawAdapter::new(
            "http://localhost".to_string(),
            "key".to_string(),
            Some(3000),
        );
        assert_eq!(adapter.timeout, Duration::from_millis(3000));
    }

    #[tokio::test]
    async fn test_dispatch_to_unreachable_returns_unavailable() {
        let adapter = make_adapter(19092);
        let result = adapter.dispatch("ping").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            AdapterError::Unavailable(_) => {}
            other => panic!("expected Unavailable, got {:?}", other),
        }
    }
}
