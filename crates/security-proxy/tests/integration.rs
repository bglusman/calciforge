/// Unit test — credential injection logic (no gateway needed)
#[tokio::test]
async fn test_credential_injection_logic() {
    use security_proxy::credentials::CredentialInjector;

    let injector = CredentialInjector::new();
    injector.add("openai", "sk-test-key-123");
    injector.add("anthropic", "sk-ant-test-456");

    // Test OpenAI header injection
    let mut headers = vec![];
    injector.inject(&mut headers, "api.openai.com").await;
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].0, "Authorization");
    assert_eq!(headers[0].1, "Bearer sk-test-key-123");

    // Test Anthropic header injection
    let mut headers = vec![];
    injector.inject(&mut headers, "api.anthropic.com").await;
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].0, "x-api-key");
    assert_eq!(headers[0].1, "sk-ant-test-456");

    // Test unknown domain (no injection)
    let mut headers = vec![];
    injector.inject(&mut headers, "example.com").await;
    assert!(headers.is_empty());
}

/// Unit test — agent config loading
#[tokio::test]
async fn test_agent_config_parsing() {
    use security_proxy::agent_config::AgentsConfig;

    let config_json = r#"{
        "agents": [{
            "agent_id": "test-agent",
            "providers": [
                {"name": "openai", "env_key": "OPENAI_API_KEY"},
                {"name": "anthropic", "env_key": "ANTHROPIC_API_KEY"}
            ],
            "proxy": {
                "enforcement": "env_var",
                "scan_outbound": true,
                "scan_inbound": true,
                "inject_credentials": true
            }
        }]
    }"#;

    let config: AgentsConfig = serde_json::from_str(config_json).unwrap();
    assert_eq!(config.agents.len(), 1);
    assert_eq!(config.agents[0].agent_id, "test-agent");
    assert_eq!(config.agents[0].providers.len(), 2);
    assert_eq!(config.agents[0].providers[0].name, "openai");

    let all_providers = config.all_providers();
    assert_eq!(all_providers.len(), 2);
}

#[tokio::test]
async fn remote_scanner_receives_provider_browsing_stripped_payload() {
    use axum::body::Body;
    use http::{Request, StatusCode};
    use security_proxy::SecurityProxy;
    use security_proxy::config::{AgentWebPolicy, GatewayConfig};
    use std::sync::Arc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
        .mount(&upstream)
        .await;

    let remote_scanner = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/scan"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "verdict": "clean",
        })))
        .mount(&remote_scanner)
        .await;

    let config = GatewayConfig {
        scan_outbound: true,
        scan_inbound: false,
        bypass_domains: vec![],
        scanner_checks: vec![adversary_detector::ScannerCheckConfig::RemoteHttp {
            url: remote_scanner.uri(),
            fail_closed: true,
        }],
        agent_web: AgentWebPolicy {
            forbid_provider_browsing: true,
            provider_browsing_strategy: "strip".to_string(),
            known_llm_apis: vec!["127.0.0.1".to_string(), "localhost".to_string()],
            ..Default::default()
        },
        ..Default::default()
    };
    let proxy = Arc::new(
        SecurityProxy::new(
            config.clone(),
            adversary_detector::ScannerConfig {
                checks: config.scanner_checks.clone(),
                ..Default::default()
            },
            adversary_detector::RateLimitConfig::default(),
        )
        .await,
    );

    let original_body = r#"{
        "model": "test-model",
        "messages": [{"role": "user", "content": "hello"}],
        "tools": [
            {"type": "web_search", "name": "web_search"},
            {"type": "function", "name": "safe_tool"}
        ]
    }"#;
    let req = Request::builder()
        .method("POST")
        .uri(format!("{}/chat/completions", upstream.uri()))
        .header("content-type", "application/json")
        .body(Body::from(original_body))
        .unwrap();

    let resp = proxy.intercept(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let upstream_requests = upstream.received_requests().await.unwrap();
    assert_eq!(upstream_requests.len(), 1);
    let upstream_body = String::from_utf8_lossy(&upstream_requests[0].body);
    assert!(
        !upstream_body.contains("web_search"),
        "provider-side browsing tools must be stripped from the upstream request"
    );
    assert!(
        upstream_body.contains("safe_tool"),
        "non-browsing tools must remain available"
    );

    let remote_requests = remote_scanner.received_requests().await.unwrap();
    assert_eq!(remote_requests.len(), 1);
    let remote_body: serde_json::Value = serde_json::from_slice(&remote_requests[0].body).unwrap();
    let remote_content = remote_body["content"].as_str().unwrap_or_default();
    assert!(
        !remote_content.contains("web_search"),
        "remote scanner must not receive a browsing tool that Calciforge stripped before forwarding"
    );
    assert!(
        remote_content.contains("safe_tool"),
        "remote scanner should inspect the same safe tool surface forwarded upstream"
    );
}
