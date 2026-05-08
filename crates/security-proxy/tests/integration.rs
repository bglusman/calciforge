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
