// Simple integration test for backend and alloy

use zeroclawed::proxy::backend::{BackendConfig, BackendType, create_backend, OneCliBackend};
use zeroclawed::proxy::openai::{ChatMessage, ModelInfo};
use zeroclawed::providers::alloy::{AlloyManager, AlloyConfig, AlloyConstituent, AlloyStrategy};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Integration Test ===");
    
    // Test 1: Create HTTP backend
    println!("1. Testing HTTP backend creation...");
    let backend_config = BackendConfig {
        backend_type: BackendType::Http,
        url: Some("https://api.deepseek.com/v1".to_string()),
        api_key: Some("sk-f4fd89ce2ce34d76bb80a9c4c0d13b08".to_string()),
        timeout_seconds: Some(30),
        ..Default::default()
    };
    
    let backend = create_backend(&backend_config)?;
    println!("   Created HTTP backend: {:?}", backend.backend_type());
    
    // Test 2: List models from backend
    println!("2. Testing model listing...");
    match backend.list_models().await {
        Ok(models) => {
            println!("   Got {} models:", models.len());
            for model in models.iter().take(3) {
                println!("   - {} ({:?})", model.id, model.provider);
            }
        }
        Err(e) => {
            println!("   Failed to list models: {}", e);
        }
    }
    
    // Test 3: Create alloy manager
    println!("3. Testing alloy manager...");
    let mut alloy_manager = AlloyManager::new();
    
    let alloy_config = AlloyConfig {
        id: "test-alloy".to_string(),
        name: "Test Alloy".to_string(),
        strategy: AlloyStrategy::Weighted,
        constituents: vec![
            AlloyConstituent {
                model: "deepseek-chat".to_string(),
                weight: 70,
            },
            AlloyConstituent {
                model: "deepseek-reasoner".to_string(),
                weight: 30,
            },
        ],
    };
    
    alloy_manager.add_alloy(alloy_config)?;
    println!("   Added alloy with 2 constituents");
    
    // Test 4: Test alloy selection
    println!("4. Testing alloy selection...");
    for i in 0..5 {
        let selection = alloy_manager.select_model("test-alloy", None);
        match selection {
            Ok(model) => println!("   Selection {}: {}", i + 1, model),
            Err(e) => println!("   Selection failed: {}", e),
        }
    }
    
    // Test 5: Simple chat completion (if API key works)
    println!("5. Testing simple chat completion...");
    let messages = vec![
        ChatMessage {
            role: "user".to_string(),
            content: "Say hello in one word.".to_string(),
            name: None,
        }
    ];
    
    match backend.chat_completion("deepseek-chat".to_string(), messages, false).await {
        Ok(response) => {
            if let Some(choice) = response.choices.first() {
                println!("   Response: {}", choice.message.content);
            } else {
                println!("   No response choice");
            }
        }
        Err(e) => {
            println!("   Chat completion failed: {}", e);
            println!("   (This is expected if API key has rate limits or other issues)");
        }
    }
    
    println!("=== Test Complete ===");
    Ok(())
}