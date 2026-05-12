use super::*;
use adversary_detector::{RateLimitConfig, ScannerConfig};
use secrets_client::{SecretAccessIdentity, SecretAccessPolicy, SecretAccessRule};

fn metadata_store(secret: &str, destinations: &[&str]) -> secrets_client::SecretMetadataStore {
    secrets_client::SecretMetadataStore {
        secrets: std::collections::BTreeMap::from([(
            secret.to_string(),
            secrets_client::SecretMetadata {
                name: secret.to_string(),
                allowed_destinations: destinations.iter().map(|value| value.to_string()).collect(),
            },
        )]),
    }
}

#[tokio::test]
async fn unknown_identity_policy_denies_before_secret_resolver() {
    let proxy = SecurityProxy::new(
        GatewayConfig {
            secret_access: SecretAccessPolicy {
                rules: vec![SecretAccessRule {
                    agents: vec!["agent-a".to_string()],
                    secrets: vec!["ALLOWED_*".to_string()],
                    ..Default::default()
                }],
            },
            ..Default::default()
        },
        ScannerConfig::default(),
        RateLimitConfig::default(),
    )
    .await;
    let metadata = metadata_store("DENIED_KEY", &["api.example.com"]);

    let err = proxy
        .resolve_and_substitute(
            "https://api.example.com/?key={{secret:DENIED_KEY}}",
            Some("api.example.com"),
            Some(&metadata),
            &SecretAccessIdentity::default(),
        )
        .await
        .expect_err("configured identity policy should fail closed without identity");

    assert!(
        err.contains("not allowed for current Calciforge identity"),
        "unknown identity denial should happen before resolver lookup; got {err}"
    );
    assert!(
        !err.contains("not found in env or fnox"),
        "secret resolver must not run when identity is unknown under configured policy: {err}"
    );
}
