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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayConfig {
    /// Port to listen on (default: 8888; override with SECURITY_PROXY_PORT)
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
        // MITM defaults to DISABLED because the default CA paths are
        // None; flipping mitm_enabled=true without a CA would fail at
        // startup (MITM needs a CA to issue leaf certs). The prior
        // `true` default was the source of a latent bug the test
        // `default_config_is_self_consistent_for_mitm` now guards
        // against. Operators who want MITM set mitm=true AND provide
        // `ca_cert_path` + `ca_key_path`.
        Self {
            port: 8888,
            mitm_enabled: false,
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

    /// The default bypass list must include loopback so that the
    /// gateway doesn't proxy traffic to itself when a local client
    /// accidentally points at the gateway. This is an invariant, not
    /// a tautology against hard-coded constants.
    #[test]
    fn default_bypass_list_includes_loopback() {
        let config = GatewayConfig::default();
        let has_loopback = config
            .bypass_domains
            .iter()
            .any(|d| d == "localhost" || d == "127.0.0.1");
        assert!(
            has_loopback,
            "default bypass list must include a loopback pattern so the \
             gateway doesn't recurse when misconfigured — got: {:?}",
            config.bypass_domains
        );
    }

    /// If MITM is enabled but no CA cert/key path is configured, the
    /// config is non-operational (MITM needs a CA to issue leaf certs).
    /// The default config must be self-consistent: either mitm=false,
    /// or both paths are Some. This catches a class of regressions
    /// where one field is flipped without the other.
    #[test]
    fn default_config_is_self_consistent_for_mitm() {
        let config = GatewayConfig::default();
        let has_ca = config.ca_cert_path.is_some() && config.ca_key_path.is_some();
        let half_set = config.ca_cert_path.is_some() ^ config.ca_key_path.is_some();
        assert!(
            !half_set,
            "CA cert/key must be both set or both None, never one-of-two: \
             cert={:?} key={:?}",
            config.ca_cert_path, config.ca_key_path
        );
        if config.mitm_enabled {
            assert!(
                has_ca,
                "mitm_enabled=true requires both ca_cert_path and \
                 ca_key_path; current default would fail to start"
            );
        }
    }

    /// Structural JSON roundtrip preserves every field. The previous
    /// test only compared `port`, so adding a field with
    /// `#[serde(skip_serializing_if)]` or forgetting `Deserialize`
    /// would slip through silently.
    #[test]
    fn config_roundtrips_through_json_preserving_every_field() {
        let config = GatewayConfig {
            port: 54321,
            mitm_enabled: false,
            ca_cert_path: Some("/tmp/ca.pem".into()),
            ca_key_path: Some("/tmp/ca.key".into()),
            scan_outbound: false,
            scan_inbound: false,
            inject_credentials: false,
            bypass_domains: vec!["a.example".into(), "b.example".into()],
            audit_log: false,
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: GatewayConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, deserialized, "roundtrip must preserve all fields");
    }

    /// Verdict variants survive JSON roundtrip with structural equality.
    /// Previously the test used `.contains("Block")` and `.contains("exfiltration")`
    /// which would pass on any string containing those substrings (e.g. a
    /// corrupted `{"Blocked":"…"}`).
    #[test]
    fn verdict_roundtrips_preserving_each_variant() {
        let cases = [
            Verdict::Allow,
            Verdict::Block {
                reason: "exfiltration detected".into(),
            },
            Verdict::Log {
                finding: "pii leak".into(),
            },
        ];
        for v in cases {
            let json = serde_json::to_string(&v).expect("serialize");
            let back: Verdict = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, v, "variant must roundtrip structurally: {v:?}");
        }
    }
}
