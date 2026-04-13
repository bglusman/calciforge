//! Test script for Helicone integration in ZeroClawed

use std::sync::Arc;

use zeroclawed::proxy::helicone_router;

fn main() -> anyhow::Result<()> {
    println!("Testing Helicone AI Gateway integration...");
    
    // Test 1: Create a local Helicone router
    println!("\nTest 1: Creating local Helicone router...");
    let config = helicone_router::HeliconeRouterConfig {
        base_url: "http://localhost:8080".to_string(),
        api_key: None,
        timeout_seconds: 120,
        router_name: Some("ai".to_string()),
        enable_caching: true,
        cache_ttl_seconds: 300,
    };
    
    match helicone_router::HeliconeRouter::new(config) {
        Ok(router) => {
            println!("✓ Successfully created HeliconeRouter");
            println!("  Base URL: {}", router.config.base_url);
            println!("  Caching enabled: {}", router.config.enable_caching);
            println!("  Cache TTL: {} seconds", router.config.cache_ttl_seconds);
        }
        Err(e) => {
            println!("✗ Failed to create HeliconeRouter: {}", e);
            return Err(e);
        }
    }
    
    // Test 2: Create a cloud Helicone router
    println!("\nTest 2: Creating cloud Helicone router...");
    match helicone_router::HeliconeRouter::cloud("test-api-key".to_string()) {
        Ok(_) => println!("✓ Successfully created cloud HeliconeRouter"),
        Err(e) => println!("✗ Failed to create cloud HeliconeRouter: {}", e),
    }
    
    // Test 3: Test default local router
    println!("\nTest 3: Creating default local router...");
    match helicone_router::HeliconeRouter::default_local() {
        Ok(_) => println!("✓ Successfully created default local HeliconeRouter"),
        Err(e) => println!("✗ Failed to create default local HeliconeRouter: {}", e),
    }
    
    println!("\n✅ All Helicone integration tests completed!");
    println!("\nNext steps:");
    println!("1. Run Helicone AI Gateway: docker run -p 8080:8080 helicone/ai-gateway");
    println!("2. Configure providers in helicone-config.yaml");
    println!("3. Update ZeroClawed config to use Helicone backend");
    println!("4. Test chat completions through the proxy");
    
    Ok(())
}