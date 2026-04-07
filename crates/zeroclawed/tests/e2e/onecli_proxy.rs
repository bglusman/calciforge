use mockito::{mock, Matcher};
use reqwest::Client;
use std::time::Duration;

#[tokio::test]
async fn test_onecli_proxy_openai_path_and_credentials() {
    // Mock upstream OpenAI endpoint
    let _m = mock("POST", "/v1/chat/completions")
        .match_header("authorization", Matcher::Exact("Bearer test-openai-token".into()))
        .match_body(Matcher::Regex(".*model.*".into()))
        .with_status(200)
        .with_body(r#"{\"id\": \"chatcmpl-1\", \"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"ok\"}}]}"#)
        .create();

    // Build OneCLI client pointing to mockito server via proxy URL
    let cfg = onecli_client::OneCliConfig {
        url: format!("http://{}/proxy/openai", &mockito::server_url()),
        agent_id: "test".to_string(),
        timeout: Duration::from_secs(5),
    };
    let client = onecli_client::OneCliClient::new(cfg).unwrap();

    let resp = client
        .post("https://api.openai.com/v1/chat/completions")
        .json(&serde_json::json!({"model":"gpt-4"}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "ok");
}

#[tokio::test]
async fn test_onecli_proxy_brave_credential_header() {
    let _m = mock("POST", "/search")
        .match_header("x-subscription-token", Matcher::Exact("brave-token".into()))
        .with_status(200)
        .with_body(r#"{\"results\": []}"#)
        .create();

    let cfg = onecli_client::OneCliConfig {
        url: format!("http://{}/proxy/brave", &mockito::server_url()),
        agent_id: "test".to_string(),
        timeout: Duration::from_secs(5),
    };
    let client = onecli_client::OneCliClient::new(cfg).unwrap();

    let resp = client
        .post("https://api.search.brave.com/search")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
}

#[tokio::test]
async fn test_onecli_proxy_path_stripping() {
    // Expect path after provider to be stripped correctly
    let _m = mock("GET", "/v1/models")
        .with_status(200)
        .with_body(r#"{\"data\": []}"#)
        .create();

    let cfg = onecli_client::OneCliConfig {
        url: format!("http://{}/proxy/openai", &mockito::server_url()),
        agent_id: "test".to_string(),
        timeout: Duration::from_secs(5),
    };
    let client = onecli_client::OneCliClient::new(cfg).unwrap();

    let resp = client
        .get("https://api.openai.com/v1/models")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
}

// Additional tests for tool call passthrough could be added here.
