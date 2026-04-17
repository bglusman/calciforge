//! Standalone test for model name normalization
//! Run with: rustc --edition 2021 test_model_normalization.rs && ./test_model_normalization

fn normalize_model_list_response(model_id: &str) -> String {
    if model_id == "kimi-for-coding" {
        "kimi/kimi-for-coding".to_string()
    } else {
        model_id.to_string()
    }
}

fn normalize_chat_request_model(model_id: &str) -> String {
    if model_id == "kimi/kimi-for-coding" {
        "kimi-for-coding".to_string()
    } else {
        model_id.to_string()
    }
}

fn main() {
    println!("Testing model name normalization...\n");
    
    // Test 1: list_models normalization (kimi-for-coding -> kimi/kimi-for-coding)
    let backend_models = vec!["kimi-for-coding", "gpt-4", "claude-3-opus"];
    println!("Test 1: list_models normalization");
    println!("  Backend returns: {:?}", backend_models);
    let normalized: Vec<String> = backend_models.iter()
        .map(|m| normalize_model_list_response(m))
        .collect();
    println!("  After normalization: {:?}", normalized);
    assert_eq!(normalized[0], "kimi/kimi-for-coding");
    assert_eq!(normalized[1], "gpt-4");
    assert_eq!(normalized[2], "claude-3-opus");
    println!("  ✓ PASSED\n");
    
    // Test 2: chat_completion normalization (kimi/kimi-for-coding -> kimi-for-coding)
    let client_models = vec!["kimi/kimi-for-coding", "gpt-4", "deepseek-chat"];
    println!("Test 2: chat_completion normalization");
    println!("  Client sends: {:?}", client_models);
    let denormalized: Vec<String> = client_models.iter()
        .map(|m| normalize_chat_request_model(m))
        .collect();
    println!("  After normalization: {:?}", denormalized);
    assert_eq!(denormalized[0], "kimi-for-coding");
    assert_eq!(denormalized[1], "gpt-4");
    assert_eq!(denormalized[2], "deepseek-chat");
    println!("  ✓ PASSED\n");
    
    // Test 3: Round-trip consistency
    println!("Test 3: Round-trip consistency");
    let original = "kimi/kimi-for-coding";
    let for_backend = normalize_chat_request_model(original);
    let back_to_client = normalize_model_list_response(&for_backend);
    println!("  Client model: {}", original);
    println!("  Backend model: {}", for_backend);
    println!("  Back to client format: {}", back_to_client);
    assert_eq!(original, back_to_client);
    println!("  ✓ PASSED\n");
    
    println!("All tests passed! ✓");
}
