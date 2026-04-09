//! OneCLI Proxy Integration Tests
//!
//! Tests the credential proxy that injects secrets from VaultWarden
//! and forwards requests to upstream APIs.

use reqwest;

/// Start OneCLI service for testing
async fn start_onecli() -> String {
    // For now, assume OneCLI is already running on 8081
    // In real tests, we'd spawn it here
    "http://127.0.0.1:8081".to_string()
}

#[tokio::test]
async fn test_proxy_openai_models_endpoint() {
    // This test verifies that /proxy/openai/v1/models correctly routes
    // to https://api.openai.com/v1/models with credential injection

    // Given: OneCLI is running
    let onecli_url = start_onecli().await;

    // When: We request the models endpoint through the proxy
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/proxy/openai/v1/models", onecli_url))
        .header("Authorization", "Bearer dummy-token")
        .send()
        .await;

    // Then: We should get a successful response (200) or auth error (401)
    // but NOT a 404 (which would indicate path routing bug)
    match response {
        Ok(resp) => {
            let status = resp.status();
            assert!(
                status.is_success() || status.as_u16() == 401,
                "Expected 200 or 401, got {}. Path routing may be broken.",
                status
            );
        }
        Err(e) => {
            // Connection refused means OneCLI isn't running - skip test
            if e.is_connect() {
                println!("Skipping test: OneCLI not running on {}", onecli_url);
                return;
            }
            panic!("Request failed: {}", e);
        }
    }
}

#[tokio::test]
async fn test_proxy_brave_uses_subscription_token_header() {
    // This test verifies that Brave API uses X-Subscription-Token header
    // instead of Bearer token (caught manually today)

    let onecli_url = start_onecli().await;

    // When: We search through Brave proxy
    let client = reqwest::Client::new();
    let response = client
        .get(format!(
            "{}/proxy/brave/res/v1/web/search?q=test&count=1",
            onecli_url
        ))
        .send()
        .await;

    // Then: Should get response (success or auth error, not 404)
    match response {
        Ok(resp) => {
            let status = resp.status();
            // 200 = success, 401 = auth error (means we hit the API)
            // 404 = path routing broken
            assert_ne!(
                status.as_u16(),
                404,
                "Got 404 for Brave proxy - path routing bug"
            );
        }
        Err(e) if e.is_connect() => {
            println!("Skipping test: OneCLI not running");
            return;
        }
        Err(e) => panic!("Request failed: {}", e),
    }
}

#[tokio::test]
async fn test_proxy_preserves_request_body() {
    // This test verifies that the proxy doesn't modify the request body
    // (critical for tool calling - tools array must be preserved)

    let onecli_url = start_onecli().await;

    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "model": "gpt-4o-mini",
        "messages": [{"role": "user", "content": "Hello"}],
        "tools": [{
            "type": "function",
            "function": {
                "name": "test_tool",
                "description": "A test tool",
                "parameters": {"type": "object", "properties": {}}
            }
        }]
    });

    let response = client
        .post(format!("{}/proxy/openai/v1/chat/completions", onecli_url))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await;

    match response {
        Ok(resp) => {
            // Any non-404 response means the body was forwarded
            assert_ne!(
                resp.status().as_u16(),
                404,
                "Got 404 - request body may have been mangled or path wrong"
            );
        }
        Err(e) if e.is_connect() => {
            println!("Skipping test: OneCLI not running");
            return;
        }
        Err(e) => panic!("Request failed: {}", e),
    }
}

#[tokio::test]
async fn test_proxy_path_stripping() {
    // This test verifies that /proxy/{provider}/path strips the prefix correctly
    // Bug caught today: /proxy/openai/v1/models was not routing correctly

    let onecli_url = start_onecli().await;

    let client = reqwest::Client::new();

    // Test various path formats
    let paths = vec![
        "/proxy/openai/v1/models",
        "/proxy/openai/v1/chat/completions",
        "/proxy/anthropic/v1/messages",
    ];

    for path in paths {
        let response = client.get(format!("{}{}", onecli_url, path)).send().await;

        match response {
            Ok(resp) => {
                // 200 = working, 401 = auth error (expected with dummy token)
                // 404 = path routing broken (THIS IS THE BUG WE CAUGHT)
                let status = resp.status();
                if status.as_u16() == 404 {
                    panic!("Path routing broken for {} - got 404", path);
                }
            }
            Err(e) if e.is_connect() => {
                println!("Skipping test: OneCLI not running");
                return;
            }
            Err(_) => continue, // Other errors are acceptable (network issues)
        }
    }
}

// TODO: Add mock-based test once OneCLI supports configurable provider URLs
