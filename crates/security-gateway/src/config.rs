#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GatewayConfig, Verdict};

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
