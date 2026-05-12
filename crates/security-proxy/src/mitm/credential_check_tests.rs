use std::sync::Arc;

use adversary_detector::{RateLimitConfig, ScannerConfig};

use super::{
    CALCIFORGE_OVERRIDE_HEADER, CalciforgeMitmHandler, MANUAL_CREDENTIAL_POLICY,
    build_credential_check_params, header_value_is_proxy_managed_secret,
    manual_credential_override_status, mitm_manual_credential_blocked_response,
    remove_calciforge_control_headers, url_with_secret_query_params_removed,
};
use crate::config::GatewayConfig;
use crate::credentials::{CredentialMapping, CredentialsConfig, InjectionMethod};
use crate::proxy::SecurityProxy;
use hudsucker::hyper::header;
use hudsucker::hyper::{Request, StatusCode};
use hudsucker::{Body as MitmBody, RequestOrResponse};

#[test]
fn credential_check_url_omits_proxy_managed_secret_query_params() {
    let url = "https://api.example.test/v1?api_key={{secret:EXAMPLE_API_KEY}}&q=books";
    let sanitized = url_with_secret_query_params_removed(url);

    assert_eq!(sanitized, "https://api.example.test/v1?q=books");
}

#[test]
fn credential_check_still_flags_manual_query_credentials() {
    let headers = header::HeaderMap::new();
    let params = build_credential_check_params(
        "https://api.example.test/v1?api_key=manual-secret&q=books",
        &headers,
    );

    assert!(ironclaw_safety::params_contain_manual_credentials(&params));
}

#[test]
fn credential_check_still_flags_mixed_manual_and_placeholder_query_credentials() {
    let headers = header::HeaderMap::new();
    let params = build_credential_check_params(
        "https://api.example.test/v1?api_key=manual-prefix-{{secret:EXAMPLE_API_KEY}}&q=books",
        &headers,
    );

    assert!(ironclaw_safety::params_contain_manual_credentials(&params));
}

#[test]
fn credential_check_allows_secret_placeholder_query_credentials() {
    let headers = header::HeaderMap::new();
    let params = build_credential_check_params(
        "https://api.example.test/v1?api_key={{secret:EXAMPLE_API_KEY}}&q=books",
        &headers,
    );

    assert!(!ironclaw_safety::params_contain_manual_credentials(&params));
}

#[test]
fn credential_check_allows_proxy_managed_auth_headers() {
    assert!(header_value_is_proxy_managed_secret(
        "{{secret:EXAMPLE_API_KEY}}"
    ));
    assert!(header_value_is_proxy_managed_secret(
        "Bearer {{secret:EXAMPLE_API_KEY}}"
    ));
}

#[test]
fn credential_check_keeps_mixed_manual_and_placeholder_headers_visible() {
    assert!(!header_value_is_proxy_managed_secret(
        "Bearer manual-prefix-{{secret:EXAMPLE_API_KEY}}"
    ));
}

#[test]
fn credential_check_flags_manual_transport_auth_headers() {
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        "Bearer provider-token".parse().unwrap(),
    );
    headers.insert(
        header::COOKIE,
        "auth_session=provider-cookie".parse().unwrap(),
    );
    headers.insert("x-api-key", "provider-token".parse().unwrap());
    headers.insert("api-key", "provider-token".parse().unwrap());

    let params = build_credential_check_params(
        "https://chatgpt.com/backend-api/accounts/check/v4-2024-04-27",
        &headers,
    );

    assert!(ironclaw_safety::params_contain_manual_credentials(&params));
}

#[test]
fn credential_check_flags_manual_transport_auth_headers_without_host_allowlist() {
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        "Bearer local-provider-token".parse().unwrap(),
    );

    let params = build_credential_check_params(
        "https://provider.example.test/v1/chat/completions",
        &headers,
    );

    assert!(ironclaw_safety::params_contain_manual_credentials(&params));
}

#[tokio::test]
async fn credential_check_allows_proxy_injected_provider_credentials() {
    let proxy = SecurityProxy::with_credentials_config(
        GatewayConfig {
            inject_credentials: true,
            scan_outbound: true,
            scan_inbound: false,
            bypass_domains: vec![],
            ..Default::default()
        },
        ScannerConfig::default(),
        RateLimitConfig::default(),
        Some(CredentialsConfig {
            mappings: vec![CredentialMapping {
                hosts: vec!["api.example.test".to_string()],
                secret_name: "OPENAI_API_KEY".to_string(),
                injection: InjectionMethod::Bearer,
            }],
            cache_ttl_secs: 0,
        }),
    )
    .await;
    proxy
        .credentials
        .add("OPENAI_API_KEY", "sk-proxy-managed-provider-token");
    let mut handler = CalciforgeMitmHandler::new(Arc::new(proxy));

    let req = Request::builder()
        .method("POST")
        .uri("https://api.example.test/v1/chat/completions")
        .header(header::CONTENT_TYPE, "application/json")
        .body(MitmBody::from(r#"{"messages":[]}"#.to_string()))
        .unwrap();

    match handler.process_request(req).await {
        RequestOrResponse::Request(forwarded) => {
            assert_eq!(
                forwarded.headers().get(header::AUTHORIZATION).unwrap(),
                "Bearer sk-proxy-managed-provider-token"
            );
        }
        RequestOrResponse::Response(response) => {
            assert_ne!(
                response.status(),
                StatusCode::FORBIDDEN,
                "proxy-managed provider credentials must be injected after manual credential checks"
            );
            panic!(
                "expected proxy-managed provider credential request to forward, got HTTP {}",
                response.status()
            );
        }
    }
}

#[test]
fn manual_credential_block_response_names_policy_and_override_requirement() {
    let response = mitm_manual_credential_blocked_response(
        "LLM-injected credential detected in outgoing request",
        "https://api.example.test/v1?api_key=redacted",
    );

    assert_eq!(
        response.headers()["X-Calciforge-Policy"],
        "ironclaw.manual_credential"
    );
    assert_eq!(
        response.headers()["X-Calciforge-Operator-Approval"],
        "required"
    );
    assert_eq!(
        response.headers()["X-Calciforge-Override-Supported"],
        "operator_scoped"
    );
    assert_eq!(
        response.headers()["X-Calciforge-Override-Header"],
        "X-Calciforge-Override"
    );
}

#[test]
fn manual_credential_override_requires_operator_approval_by_default() {
    let mut headers = header::HeaderMap::new();
    headers.insert(
        CALCIFORGE_OVERRIDE_HEADER,
        header::HeaderValue::from_static(MANUAL_CREDENTIAL_POLICY),
    );

    let status = manual_credential_override_status(&headers, true);
    assert!(!status.allowed);
    assert_eq!(status.reason, "operator approval token required");
}

#[test]
fn manual_credential_override_can_be_configured_without_operator_approval() {
    let mut headers = header::HeaderMap::new();
    headers.insert(
        CALCIFORGE_OVERRIDE_HEADER,
        header::HeaderValue::from_static(MANUAL_CREDENTIAL_POLICY),
    );

    let status = manual_credential_override_status(&headers, false);
    assert!(status.allowed);
}

#[test]
fn calciforge_control_headers_are_stripped_before_forwarding() {
    let mut headers = header::HeaderMap::new();
    headers.insert(
        "x-calciforge-override",
        header::HeaderValue::from_static(MANUAL_CREDENTIAL_POLICY),
    );
    headers.insert(
        "x-calciforge-anything",
        header::HeaderValue::from_static("control-plane"),
    );
    headers.insert(
        "x-agent-id",
        header::HeaderValue::from_static("legacy-agent"),
    );
    headers.insert(
        "x-upstream-header",
        header::HeaderValue::from_static("keep"),
    );

    remove_calciforge_control_headers(&mut headers);

    assert!(!headers.contains_key("x-calciforge-override"));
    assert!(!headers.contains_key("x-calciforge-anything"));
    assert!(!headers.contains_key("x-agent-id"));
    assert_eq!(headers["x-upstream-header"], "keep");
}
