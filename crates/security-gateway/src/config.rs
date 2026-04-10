use serde::{Deserialize, Serialize};

/// What action to take for a request/response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Verdict {
    /// Allow the traffic through.
    Allow,
    /// Block the traffic with a reason.
    Block { reason: String },
    /// Allow but log the finding.
    Log { finding: String },
}

/// Result of scanning outbound request content (exfiltration check).
#[derive(Debug, Clone)]
pub struct ExfilReport {
    pub verdict: Verdict,
    pub findings: Vec<String>,
    pub scan_time_ms: u64,
}

/// Result of scanning inbound response content (injection check).
#[derive(Debug, Clone)]
pub struct InjectionReport {
    pub verdict: Verdict,
    pub findings: Vec<String>,
    pub scan_time_ms: u64,
}

/// Configuration for the security gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    /// Port to listen on (default: 8080)
    pub port: u16,
    /// Whether to perform MITM for HTTPS (requires CA cert trusted by clients)
    pub mitm_enabled: bool,
    /// Path to CA certificate PEM (for MITM)
    pub ca_cert_path: Option<String>,
    /// Path to CA private key PEM
    pub ca_key_path: Option<String>,
    /// Enable exfiltration scanning on outbound requests
    pub scan_outbound: bool,
    /// Enable injection scanning on inbound responses
    pub scan_inbound: bool,
    /// Enable credential injection from env/vault
    pub inject_credentials: bool,
    /// Domains that bypass the gateway entirely
    pub bypass_domains: Vec<String>,
    /// Log all traffic (even allowed) for audit
    pub audit_log: bool,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            port: 8080,
            mitm_enabled: true,
            ca_cert_path: None,
            ca_key_path: None,
            scan_outbound: true,
            scan_inbound: true,
            inject_credentials: true,
            bypass_domains: vec![
                "localhost".into(),
                "127.0.0.1".into(),
                "192.168.1.*".into(),
                "10.*.*.*".into(),
            ],
            audit_log: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = GatewayConfig::default();
        assert_eq!(config.port, 8080);
        assert!(config.mitm_enabled);
        assert!(config.scan_outbound);
        assert!(config.scan_inbound);
        assert!(config.inject_credentials);
        assert!(config.audit_log);
        assert!(!config.bypass_domains.is_empty());
    }

    #[test]
    fn test_config_serialization() {
        let config = GatewayConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: GatewayConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.port, deserialized.port);
    }

    #[test]
    fn test_verdict_equality() {
        assert_eq!(Verdict::Allow, Verdict::Allow);
        assert_ne!(
            Verdict::Allow,
            Verdict::Block {
                reason: "test".into()
            }
        );
    }

    #[test]
    fn test_verdict_serialization() {
        let v = Verdict::Block {
            reason: "exfiltration detected".into(),
        };
        let json = serde_json::to_string(&v).unwrap();
        assert!(json.contains("Block"));
        assert!(json.contains("exfiltration"));
    }
}
