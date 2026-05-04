//! Thin wrapper around `ironclaw_safety` for use in the security proxy.
//!
//! Provides credential-injection detection on outgoing requests and
//! secret-leak scanning on inbound response bodies.

use ironclaw_safety::{LeakDetector, SafetyConfig, SafetyLayer};
use tracing::debug;

pub struct IronclawSafety {
    #[allow(dead_code)]
    safety: SafetyLayer,
    leak_detector: LeakDetector,
}

impl Default for IronclawSafety {
    fn default() -> Self {
        Self::new()
    }
}

impl IronclawSafety {
    pub fn new() -> Self {
        let config = SafetyConfig {
            max_output_length: 1_000_000,
            injection_check_enabled: true,
        };
        Self {
            safety: SafetyLayer::new(&config),
            leak_detector: LeakDetector::new(),
        }
    }

    /// Check if outgoing request headers/URL contain manually-injected credentials
    /// that weren't placed by the proxy's credential injector.
    pub fn check_request_credentials(&self, params: &serde_json::Value) -> Result<(), String> {
        if ironclaw_safety::params_contain_manual_credentials(params) {
            return Err("LLM-injected credential detected in outgoing request".to_string());
        }

        debug!(
            "credential check passed for {}",
            params["url"].as_str().unwrap_or("?")
        );
        Ok(())
    }

    /// Scan a response body for leaked secrets.
    pub fn scan_response_body(&self, body: &str) -> Result<(), String> {
        let result = self.leak_detector.scan(body);
        if !result.is_clean() {
            let descriptions: Vec<_> = result
                .matches
                .iter()
                .map(|m| format!("{:?}: {}", m.severity, m.pattern_name))
                .collect();
            return Err(format!(
                "secret leak detected in response: {}",
                descriptions.join(", ")
            ));
        }
        Ok(())
    }
}
