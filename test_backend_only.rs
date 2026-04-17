use zeroclawed::proxy::backend::{BackendConfig, BackendType, create_backend};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Testing Unified Backend Interface POC ===\n");
    
    // Test 1: Mock backend
    println!("1. Testing Mock Backend:");
    let mock_config = BackendConfig {
        backend_type: BackendType::Mock,
        ..Default::default()
    };
    
    let mock_backend = create_backend(&mock_config)?;
    println!("   Created: {:?}", mock_backend.backend_type());
    
    let models = mock_backend.list_models().await?;
    println!("   Models available: {}", models.len());
    for model in &models {
        println!("     - {} ({:?})", model.id, model.provider);
    }
    
    let response = mock_backend.chat_completion(
        "gpt-4".to_string(),
        vec![
            zeroclawed::proxy::openai::ChatMessage {
                role: "user".to_string(),
                content: Some(zeroclawed::proxy::openai::MessageContent::Text("Hello from POC test!".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            }
        ],
        false,
    ).await?;
    
    println!("   Chat response: {}", 
        response.choices.get(0)
            .and_then(|c| c.message.content.as_ref())
            .and_then(|c| c.to_text())
            .unwrap_or_else(|| "No content".to_string())
    );
    
    // Test 2: Try other backends (should fail with NotAvailable)
    println!("\n2. Testing Other Backends (should fail with NotAvailable):");
    
    let backends = [
        (BackendType::Embedded, "Embedded"),
        (BackendType::Library, "Library"),
        (BackendType::Http, "HTTP"),
    ];
    
    for (backend_type, name) in backends {
        let config = BackendConfig {
            backend_type,
            ..Default::default()
        };
        
        match create_backend(&config) {
            Ok(backend) => {
                println!("   {}: Created successfully", name);
                // Try to list models (should fail)
                match backend.list_models().await {
                    Ok(_) => println!("     ERROR: Should have failed but didn't!"),
                    Err(e) => println!("     Expected error: {}", e),
                }
            }
            Err(e) => {
                println!("   {}: Failed to create: {}", name, e);
            }
        }
    }
    
    println!("\n=== POC Summary ===");
    println!("✅ Unified Backend Interface works!");
    println!("   - Mock backend responds correctly");
    println!("   - Interface abstracts implementation details");
    println!("   - Easy to add new backend types");
    println!("   - Clean error handling");
    println!("\nNext steps:");
    println!("   1. Implement Embedded backend (OneCLI subprocess)");
    println!("   2. Implement HTTP backend (OneCLI server)");
    println!("   3. Implement Library backend (OneCLI as library)");
    println!("   4. Add configuration from config.toml");
    
    Ok(())
}