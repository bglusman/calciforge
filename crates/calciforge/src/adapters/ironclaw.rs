//! Native IronClaw adapter — connects to a running IronClaw instance via
//! its HTTP webhook channel.
//!
//! IronClaw v0.27.0 exposes:
//! - `POST /webhook` — submit a message (synchronous when `wait_for_response: true`)
//! - `GET /health` — health check
//!
//! Authentication is via HMAC-SHA256 signature. Both `X-IronClaw-Signature` (source builds)
//! and `x-hub-signature-256` (release binaries) headers are sent for compatibility.

use std::time::Duration;

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use super::{AdapterError, AgentAdapter, DispatchContext};

const DEFAULT_TIMEOUT_MS: u64 = 300_000;

type HmacSha256 = Hmac<Sha256>;

pub struct IronClawAdapter {
    client: reqwest::Client,
    endpoint: String,
    webhook_secret: String,
    #[allow(dead_code)]
    model: Option<String>,
}

impl IronClawAdapter {
    pub fn new(
        endpoint: String,
        webhook_secret: String,
        model: Option<String>,
        timeout_ms: Option<u64>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_millis(
                timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS),
            ))
            .build()
            .expect("reqwest client");

        Self {
            client,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            webhook_secret,
            model,
        }
    }

    fn webhook_url(&self) -> String {
        format!("{}/webhook", self.endpoint)
    }

    fn sign_body(&self, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(self.webhook_secret.as_bytes()).expect("HMAC key");
        mac.update(body);
        let result = mac.finalize().into_bytes();
        format!("sha256={}", hex::encode(result))
    }

    async fn send_message(
        &self,
        message: &str,
        sender: Option<&str>,
        thread_id: Option<&str>,
    ) -> Result<WebhookResponse, AdapterError> {
        let body = WebhookRequest {
            content: message.to_string(),
            user_id: sender.map(|s| s.to_string()),
            thread_id: thread_id.map(|s| s.to_string()),
            wait_for_response: true,
            attachments: vec![],
        };

        let body_bytes = serde_json::to_vec(&body)
            .map_err(|e| AdapterError::Protocol(format!("Failed to serialize request: {e}")))?;

        let signature = self.sign_body(&body_bytes);

        let resp = self
            .client
            .post(self.webhook_url())
            .header("Content-Type", "application/json")
            .header("X-IronClaw-Signature", &signature)
            .header("x-hub-signature-256", &signature)
            .body(body_bytes)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    AdapterError::Timeout
                } else {
                    AdapterError::Unavailable(format!("IronClaw webhook failed: {e}"))
                }
            })?;

        let status = resp.status();
        let resp_body = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(AdapterError::Protocol(format!(
                "IronClaw POST /webhook returned {status}: {resp_body}"
            )));
        }

        let webhook_resp: WebhookResponse = serde_json::from_str(&resp_body).map_err(|e| {
            AdapterError::Protocol(format!(
                "IronClaw response parse error: {e} — body: {resp_body}"
            ))
        })?;

        if webhook_resp.status == "error" {
            let msg = webhook_resp
                .response
                .unwrap_or_else(|| "unknown error".into());
            return Err(AdapterError::Protocol(format!(
                "IronClaw returned error: {msg}"
            )));
        }

        Ok(webhook_resp)
    }
}

#[async_trait]
impl AgentAdapter for IronClawAdapter {
    async fn dispatch(&self, msg: &str) -> Result<String, AdapterError> {
        let resp = self.send_message(msg, None, None).await?;
        resp.response.ok_or_else(|| {
            AdapterError::Protocol("IronClaw returned success but no response text".to_string())
        })
    }

    async fn dispatch_with_context(
        &self,
        ctx: DispatchContext<'_>,
    ) -> Result<String, AdapterError> {
        let resp = self
            .send_message(ctx.message, ctx.sender, ctx.session)
            .await?;
        resp.response.ok_or_else(|| {
            AdapterError::Protocol("IronClaw returned success but no response text".to_string())
        })
    }

    fn kind(&self) -> &'static str {
        "ironclaw"
    }
}

// ── Wire types ──────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct WebhookRequest {
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_id: Option<String>,
    wait_for_response: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<AttachmentData>,
}

#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct AttachmentData {
    mime_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data_base64: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WebhookResponse {
    #[allow(dead_code)]
    message_id: String,
    status: String,
    response: Option<String>,
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_url_construction() {
        let adapter = IronClawAdapter::new(
            "http://localhost:3000".to_string(),
            "test-secret".to_string(),
            None,
            None,
        );
        assert_eq!(adapter.webhook_url(), "http://localhost:3000/webhook");
    }

    #[test]
    fn webhook_url_strips_trailing_slash() {
        let adapter = IronClawAdapter::new(
            "http://localhost:3000/".to_string(),
            "test-secret".to_string(),
            None,
            None,
        );
        assert_eq!(adapter.webhook_url(), "http://localhost:3000/webhook");
    }

    #[test]
    fn hmac_signature_format() {
        let adapter = IronClawAdapter::new(
            "http://localhost:3000".to_string(),
            "my-secret".to_string(),
            None,
            None,
        );
        let sig = adapter.sign_body(b"test body");
        assert!(sig.starts_with("sha256="));
        assert_eq!(sig.len(), 7 + 64); // "sha256=" + 64 hex chars
    }

    #[test]
    fn hmac_signature_is_deterministic() {
        let adapter = IronClawAdapter::new(
            "http://localhost:3000".to_string(),
            "secret".to_string(),
            None,
            None,
        );
        let sig1 = adapter.sign_body(b"hello");
        let sig2 = adapter.sign_body(b"hello");
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn hmac_signature_differs_for_different_bodies() {
        let adapter = IronClawAdapter::new(
            "http://localhost:3000".to_string(),
            "secret".to_string(),
            None,
            None,
        );
        let sig1 = adapter.sign_body(b"hello");
        let sig2 = adapter.sign_body(b"world");
        assert_ne!(sig1, sig2);
    }

    #[test]
    fn webhook_request_serialization() {
        let req = WebhookRequest {
            content: "hello".to_string(),
            user_id: Some("brian".to_string()),
            thread_id: None,
            wait_for_response: true,
            attachments: vec![],
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["content"], "hello");
        assert_eq!(json["user_id"], "brian");
        assert_eq!(json["wait_for_response"], true);
        assert!(json.get("thread_id").is_none());
        assert!(json.get("attachments").is_none());
    }

    #[test]
    fn webhook_response_deserialization() {
        let json = r#"{"message_id":"abc-123","status":"ok","response":"Hi there!"}"#;
        let resp: WebhookResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.status, "ok");
        assert_eq!(resp.response.as_deref(), Some("Hi there!"));
    }

    #[test]
    fn webhook_response_without_response_field() {
        let json = r#"{"message_id":"abc-123","status":"accepted","response":null}"#;
        let resp: WebhookResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.status, "accepted");
        assert!(resp.response.is_none());
    }
}
