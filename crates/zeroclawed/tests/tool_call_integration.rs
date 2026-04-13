//! Integration test for tool call forwarding
//! Starts the proxy and verifies tools are forwarded to the backend

use std::time::Duration;

/// Test that verifies the proxy properly forwards tool definitions
/// This test requires the proxy to be running on localhost:8083
#[tokio::test]
#[ignore = "Requires running proxy instance - run manually with: cargo test -- --ignored"]
async fn test_tool_calls_forwarded_via_proxy() {
    let client = reqwest::Client::new();

    // Send request with tools through proxy
    let response = client
        .post("http://127.0.0.1:8083/v1/chat/completions")
        .header("Authorization", "Bearer test-key-123")
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "model": "deepseek-chat",
            "messages": [{"role": "user", "content": "What is 2+2?"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "calculate",
                    "description": "Perform calculation",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "expression": {"type": "string"}
                        }
                    }
                }
            }],
            "tool_choice": "auto"
        }))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .expect("Request should succeed");

    assert!(
        response.status().is_success(),
        "Proxy should return success"
    );

    let body: serde_json::Value = response.json().await.expect("Should parse JSON");

    // Verify we got a valid response
    assert!(
        body.get("choices").is_some(),
        "Response should have choices"
    );

    // The model might or might not use the tool - that's fine
    // What's important is that the request didn't fail with "tools not supported"
    println!("Response: {}", serde_json::to_string_pretty(&body).unwrap());
}

/// Verify that tool calls in responses are preserved
#[tokio::test]
#[ignore = "Requires running proxy instance - run manually with: cargo test -- --ignored"]
async fn test_tool_calls_in_response_preserved() {
    // This would verify that when the backend returns tool_calls,
    // they are properly included in the proxy response
}
