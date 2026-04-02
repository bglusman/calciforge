//! Signal webhook integration for human-in-the-loop approvals (P3-18)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use crate::approval::token::{hash_token, TokenAuditInfo};

/// Signal webhook payload for approval confirmations
#[derive(Debug, Clone, Deserialize)]
pub struct SignalWebhookPayload {
    /// 16-char approval token
    pub token: String,
    /// Confirmation code (e.g., "CONFIRM")
    pub confirmation_code: String,
    /// Signal number that confirmed (e.g., "+15555550001")
    pub approver: String,
    /// Timestamp of confirmation
    pub timestamp: DateTime<Utc>,
}

/// Validated approval from Signal
#[derive(Debug, Clone)]
pub struct ValidatedApproval {
    pub token_hash: String,
    pub approver: String,
    pub timestamp: DateTime<Utc>,
}

/// Signal integration client
pub struct SignalClient {
    webhook_url: String,
    http_client: reqwest::Client,
    allowed_approvers: Vec<String>, // List of approved Signal numbers
}

impl SignalClient {
    /// Create new Signal client
    pub fn new(webhook_url: String, allowed_approvers: Vec<String>) -> Self {
        Self {
            webhook_url,
            http_client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("Failed to build HTTP client"),
            allowed_approvers,
        }
    }

    /// Send approval request notification via Signal
    pub async fn notify_approval_request(
        &self,
        token_audit: &TokenAuditInfo,
        caller: &str,
        operation: &str,
        target: &str,
    ) -> anyhow::Result<()> {
        if self.webhook_url.is_empty() {
            debug!("No Signal webhook configured, skipping notification");
            return Ok(());
        }

        let message = format!(
            "🔐 PolyClaw Approval Request\n\n\
            Agent: {}\n\
            Operation: {}\n\
            Target: {}\n\n\
            Reply CONFIRM {} to approve (5 min timeout)\n\
            Token: {}",
            caller, operation, target, token_audit.masked, token_audit.masked
        );

        let payload = serde_json::json!({
            "message": message,
            "recipients": self.allowed_approvers,
            "token_hash_prefix": &token_audit.hash_prefix,
        });

        info!(
            caller = %caller,
            operation = %operation,
            target = %target,
            "Sending Signal approval request"
        );

        let response = self.http_client
            .post(&self.webhook_url)
            .json(&payload)
            .send()
            .await;

        match response {
            Ok(resp) if resp.status().is_success() => {
                info!("Signal notification sent successfully");
                Ok(())
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                error!(status = %status, body = %body, "Signal webhook returned error");
                Err(anyhow::anyhow!("Signal webhook error: {}", status))
            }
            Err(e) => {
                error!(error = %e, "Failed to send Signal notification");
                Err(e.into())
            }
        }
    }

    /// Validate a webhook callback from Signal
    pub fn validate_callback(&self, payload: &SignalWebhookPayload) -> anyhow::Result<ValidatedApproval> {
        // Check if approver is in allowlist
        if !self.allowed_approvers.contains(&payload.approver) {
            warn!(
                approver = %payload.approver,
                "Approval attempt from unauthorized Signal number"
            );
            return Err(anyhow::anyhow!("Unauthorized approver: {}", payload.approver));
        }

        // Validate confirmation code (case-insensitive)
        let code = payload.confirmation_code.to_uppercase();
        if code != "CONFIRM" && code != "YES" && code != "APPROVE" {
            return Err(anyhow::anyhow!(
                "Invalid confirmation code: {}",
                payload.confirmation_code
            ));
        }

        // Check timestamp is recent (5 minute window)
        let age = Utc::now() - payload.timestamp;
        if age.num_seconds() > 300 {
            return Err(anyhow::anyhow!("Confirmation expired (older than 5 minutes)"));
        }

        // Hash the token for storage
        let token_hash = hash_token(&payload.token);

        info!(
            approver = %payload.approver,
            token_hash_prefix = %&token_hash[..8],
            "Signal approval validated"
        );

        Ok(ValidatedApproval {
            token_hash,
            approver: payload.approver.clone(),
            timestamp: payload.timestamp,
        })
    }

    /// Verify that an approver is allowed
    pub fn is_allowed_approver(&self, number: &str) -> bool {
        self.allowed_approvers.iter().any(|a| a == number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_client() -> SignalClient {
        SignalClient::new(
            "https://example.com/webhook".to_string(),
            vec!["+15555550001".to_string(), "+15551234567".to_string()],
        )
    }

    #[test]
    fn test_validate_callback_valid() {
        let client = test_client();
        let payload = SignalWebhookPayload {
            token: "X7K9M2P4Q8R5N6V3".to_string(),
            confirmation_code: "CONFIRM".to_string(),
            approver: "+15555550001".to_string(),
            timestamp: Utc::now(),
        };

        let result = client.validate_callback(&payload);
        assert!(result.is_ok());

        let approval = result.unwrap();
        assert_eq!(approval.approver, "+15555550001");
    }

    #[test]
    fn test_validate_callback_unauthorized_approver() {
        let client = test_client();
        let payload = SignalWebhookPayload {
            token: "X7K9M2P4Q8R5N6V3".to_string(),
            confirmation_code: "CONFIRM".to_string(),
            approver: "+19999999999".to_string(), // Not in allowlist
            timestamp: Utc::now(),
        };

        let result = client.validate_callback(&payload);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unauthorized"));
    }

    #[test]
    fn test_validate_callback_expired() {
        let client = test_client();
        let payload = SignalWebhookPayload {
            token: "X7K9M2P4Q8R5N6V3".to_string(),
            confirmation_code: "CONFIRM".to_string(),
            approver: "+15555550001".to_string(),
            timestamp: Utc::now() - chrono::Duration::minutes(10), // Expired
        };

        let result = client.validate_callback(&payload);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("expired"));
    }

    #[test]
    fn test_validate_callback_case_insensitive() {
        let client = test_client();
        
        for code in &["confirm", "Confirm", "CONFIRM", "yes", "YES", "approve", "APPROVE"] {
            let payload = SignalWebhookPayload {
                token: "X7K9M2P4Q8R5N6V3".to_string(),
                confirmation_code: code.to_string(),
                approver: "+15555550001".to_string(),
                timestamp: Utc::now(),
            };

            let result = client.validate_callback(&payload);
            assert!(result.is_ok(), "Failed for code: {}", code);
        }
    }
}
