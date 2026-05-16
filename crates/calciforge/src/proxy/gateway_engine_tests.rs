use super::gateway::{GatewayType, openai_compatible_headers};
use crate::config::GatewayRetryConfig;

#[test]
fn helicone_policy_headers_are_overlay_not_separate_gateway_core() {
    let retry = GatewayRetryConfig {
        enabled: true,
        max_retries: 4,
        min_timeout_ms: 250,
        max_timeout_ms: 3_000,
        factor: 3,
        retry_on: vec![],
    };

    let headers = openai_compatible_headers(GatewayType::Helicone, Some("test-key"), &retry, None)
        .expect("helicone overlay should add headers");

    assert_eq!(
        headers.get("helicone-auth"),
        Some(&"Bearer test-key".to_string())
    );
    assert_eq!(
        headers.get("helicone-retry-enabled"),
        Some(&"true".to_string())
    );
    assert_eq!(headers.get("helicone-retry-num"), Some(&"4".to_string()));
    assert_eq!(
        headers.get("helicone-retry-min-timeout"),
        Some(&"250".to_string())
    );
    assert_eq!(
        headers.get("helicone-retry-max-timeout"),
        Some(&"3000".to_string())
    );
    assert_eq!(headers.get("helicone-retry-factor"), Some(&"3".to_string()));

    assert!(
        openai_compatible_headers(
            GatewayType::LiteLlm,
            Some("test-key"),
            &GatewayRetryConfig::default(),
            None
        )
        .is_none(),
        "LiteLLM should not inherit Helicone-specific headers"
    );
}
