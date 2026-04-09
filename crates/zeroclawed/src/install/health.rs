//! HTTP health-check support for the ZeroClawed installer.
//!
//! Each claw exposes an endpoint that ZeroClawed can poll to verify it's alive.
//! Health checks are performed:
//! - Before installation (baseline)
//! - After applying config changes (post-apply verification)
//!
//! Cli-adapter claws have no network endpoint and skip health checks.

use anyhow::{bail, Context, Result};
use std::time::Duration;

use super::model::ClawKind;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Abstract health-checker so tests can inject a mock.
#[async_trait::async_trait]
pub trait HealthChecker: Send + Sync {
    /// Check whether the endpoint is reachable and healthy.
    ///
    /// Returns `Ok(())` if healthy, `Err` with a descriptive message otherwise.
    async fn check(&self, endpoint: &str) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Real implementation
// ---------------------------------------------------------------------------

/// HTTP health-checker using `reqwest`.
///
/// Sends a GET to `<endpoint>/health` (or `<endpoint>/up`, falling back to
/// `<endpoint>` itself). A 2xx response is considered healthy.
pub struct HttpHealthChecker {
    timeout: Duration,
}

impl HttpHealthChecker {
    /// Create with default 10-second timeout.
    pub fn new() -> Self {
        Self {
            timeout: Duration::from_secs(10),
        }
    }

}

impl Default for HttpHealthChecker {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl HealthChecker for HttpHealthChecker {
    async fn check(&self, endpoint: &str) -> Result<()> {
        let client = reqwest::Client::builder()
            .timeout(self.timeout)
            .build()
            .context("failed to build HTTP client for health check")?;

        // Try <endpoint>/health first, then /up, then the root endpoint.
        let candidates = [
            format!("{}/health", endpoint.trim_end_matches('/')),
            format!("{}/up", endpoint.trim_end_matches('/')),
            endpoint.to_string(),
        ];

        let mut last_err: Option<anyhow::Error> = None;
        for url in &candidates {
            match client.get(url).send().await {
                Ok(resp) if resp.status().is_success() || resp.status().is_redirection() => {
                    return Ok(());
                }
                Ok(resp) => {
                    last_err = Some(anyhow::anyhow!(
                        "health check GET {} returned {}",
                        url,
                        resp.status()
                    ));
                }
                Err(e) => {
                    last_err = Some(anyhow::anyhow!("health check GET {} failed: {}", url, e));
                }
            }
        }

        bail!(
            "all health check URLs failed for endpoint '{}': {}",
            endpoint,
            last_err.map(|e| e.to_string()).unwrap_or_default()
        )
    }
}

// ---------------------------------------------------------------------------
// Mock implementation
// ---------------------------------------------------------------------------

/// Canned health check responses for tests.
///
/// Supports two modes per endpoint:
/// - Static: always returns the same result (`set_healthy` / `set_unhealthy`)
/// - Sequential: returns responses in order (`push_response`), then falls back to static or Ok
pub struct MockHealthChecker {
    /// Map of endpoint → static (is_healthy, message).
    static_responses: std::sync::Mutex<std::collections::HashMap<String, (bool, String)>>,
    /// Ordered queue of responses consumed in FIFO order.
    queued: std::sync::Mutex<Vec<(bool, String)>>,
    /// Calls recorded for assertions.
    pub calls: std::sync::Mutex<Vec<String>>,
}

impl MockHealthChecker {
    pub fn new() -> Self {
        Self {
            static_responses: std::sync::Mutex::new(std::collections::HashMap::new()),
            queued: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Register a healthy static response for an endpoint.
    pub fn set_healthy(&self, endpoint: &str) {
        self.static_responses
            .lock()
            .unwrap()
            .insert(endpoint.to_string(), (true, String::new()));
    }

    /// Register an unhealthy static response for an endpoint.
    pub fn set_unhealthy(&self, endpoint: &str, message: &str) {
        self.static_responses
            .lock()
            .unwrap()
            .insert(endpoint.to_string(), (false, message.to_string()));
    }

    /// Enqueue a sequential response (consumed in FIFO order regardless of endpoint).
    /// When the queue is exhausted, falls back to static responses (or Ok).
    pub fn push_ok(&self) {
        self.queued.lock().unwrap().push((true, String::new()));
    }

    pub fn push_err(&self, message: &str) {
        self.queued
            .lock()
            .unwrap()
            .push((false, message.to_string()));
    }

    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

impl Default for MockHealthChecker {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl HealthChecker for MockHealthChecker {
    async fn check(&self, endpoint: &str) -> Result<()> {
        self.calls.lock().unwrap().push(endpoint.to_string());

        // Consume from queue first.
        let queued_response = self.queued.lock().unwrap().first().cloned();
        if let Some((ok, msg)) = queued_response {
            self.queued.lock().unwrap().remove(0);
            return if ok {
                Ok(())
            } else {
                bail!("health check failed: {}", msg)
            };
        }

        // Fall back to static responses.
        let static_responses = self.static_responses.lock().unwrap();
        match static_responses.get(endpoint) {
            Some((true, _)) => Ok(()),
            Some((false, msg)) => bail!("health check failed: {}", msg),
            None => Ok(()), // default: healthy if not configured
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns `true` if this adapter supports a network health check.
///
/// `Cli` adapters don't expose a network endpoint.
pub fn supports_health_check(adapter: &ClawKind) -> bool {
    !matches!(adapter, ClawKind::Cli { .. })
}

/// Perform a health check for a claw target.
///
/// - Skips (returns `Ok(())`) for `Cli` adapters.
/// - For all others, delegates to `checker.check(endpoint)`.
pub async fn health_check_claw(
    checker: &dyn HealthChecker,
    adapter: &ClawKind,
    endpoint: &str,
) -> Result<()> {
    if !supports_health_check(adapter) {
        return Ok(());
    }
    checker.check(endpoint).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_health_check_healthy() {
        let checker = MockHealthChecker::new();
        checker.set_healthy("http://localhost:18799");
        let result = checker.check("http://localhost:18799").await;
        assert!(result.is_ok());
        assert_eq!(checker.call_count(), 1);
    }

    #[tokio::test]
    async fn mock_health_check_unhealthy() {
        let checker = MockHealthChecker::new();
        checker.set_unhealthy("http://localhost:18799", "connection refused");
        let result = checker.check("http://localhost:18799").await;
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(msg.contains("connection refused"), "got: {}", msg);
    }

    #[tokio::test]
    async fn mock_health_check_default_healthy() {
        // Endpoints not explicitly registered default to healthy
        let checker = MockHealthChecker::new();
        let result = checker.check("http://unknown-endpoint/").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn health_check_claw_skips_cli() {
        let checker = MockHealthChecker::new();
        checker.set_unhealthy("anything", "should not be called");

        let adapter = ClawKind::Cli {
            command: "my-claw".into(),
        };
        let result = health_check_claw(&checker, &adapter, "anything").await;
        assert!(result.is_ok(), "Cli adapter should skip health check");
        assert_eq!(
            checker.call_count(),
            0,
            "checker should not be called for Cli"
        );
    }

    #[tokio::test]
    async fn health_check_claw_nzc_checks_endpoint() {
        let checker = MockHealthChecker::new();
        checker.set_healthy("http://host:18799");

        let adapter = ClawKind::NzcNative;
        let result = health_check_claw(&checker, &adapter, "http://host:18799").await;
        assert!(result.is_ok());
        assert_eq!(checker.call_count(), 1);
    }

    #[tokio::test]
    async fn health_check_claw_openclaw_checks_endpoint() {
        let checker = MockHealthChecker::new();
        checker.set_healthy("http://host:18789");

        let adapter = ClawKind::OpenClawHttp;
        let result = health_check_claw(&checker, &adapter, "http://host:18789").await;
        assert!(result.is_ok());
        assert_eq!(checker.call_count(), 1);
    }

    #[tokio::test]
    async fn health_check_claw_webhook_checks_endpoint() {
        let checker = MockHealthChecker::new();
        checker.set_healthy("http://hook.internal/receive");

        let adapter = ClawKind::Webhook {
            endpoint: "http://hook.internal/receive".into(),
            format: crate::install::model::WebhookFormat::Json,
        };
        let result = health_check_claw(&checker, &adapter, "http://hook.internal/receive").await;
        assert!(result.is_ok());
        assert_eq!(checker.call_count(), 1);
    }

    #[test]
    fn supports_health_check_cli_false() {
        assert!(!supports_health_check(&ClawKind::Cli {
            command: "x".into()
        }));
    }

    #[test]
    fn supports_health_check_others_true() {
        assert!(supports_health_check(&ClawKind::NzcNative));
        assert!(supports_health_check(&ClawKind::OpenClawHttp));
        assert!(supports_health_check(&ClawKind::OpenAiCompat {
            endpoint: "http://x".into()
        }));
        assert!(supports_health_check(&ClawKind::Webhook {
            endpoint: "http://x".into(),
            format: crate::install::model::WebhookFormat::Json,
        }));
    }
}
