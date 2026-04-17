// Simple test to verify the backend POC works
use zeroclawed::proxy::backend::{BackendConfig, BackendType, create_backend};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Testing Unified Backend POC...");
    
    // Test Mock backend
    let config = BackendConfig {
        backend_type: BackendType::Mock,
        ..Default::default()
    };
    
    let backend = create_backend(&config)?;
    println!("Created backend: {:?}", backend.backend_type());
    
    // Test listing models
    let models = backend.list_models().await?;
    println!("Available models from backend:");
    for model in models {
        println!("  - {} ({:?})", model.id, model.provider);
    }
    
    // Test chat completion
    let response = backend.chat_completion(
        "gpt-4".to_string(),
        vec![
            zeroclawed::proxy::openai::ChatMessage {
                role: "user".to_string(),
                content: Some(zeroclawed::proxy::openai::MessageContent::Text("Hello, world!".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            }
        ],
        false,
    ).await?;
    
    println!("Chat completion response:");
    println!("  ID: {}", response.id);
    println!("  Model: {}", response.model);
    if let Some(choice) = response.choices.get(0) {
        if let Some(content) = &choice.message.content {
            println!("  Response: {}", content.to_text().unwrap_or_default());
        }
    }
    
    println!("\n✅ POC Backend test passed!");
    Ok(())
}