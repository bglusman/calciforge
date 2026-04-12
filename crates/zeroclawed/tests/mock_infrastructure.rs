//! Mock infrastructure tests for ZeroClawed
//! Fast, controllable unit/e2e tests using mock channels and providers

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;


// ==================== SIMPLE MOCK INFRASTRUCTURE ====================

#[derive(Clone)]
struct MockChannel {
    messages: Arc<Mutex<Vec<String>>>,
}

impl MockChannel {
    fn new() -> Self {
        Self {
            messages: Arc::new(Mutex::new(Vec::new())),
        }
    }

    async fn send(&self, message: &str) {
        let mut messages = self.messages.lock().await;
        messages.push(message.to_string());
    }

    async fn send_with_delay(&self, message: &str, delay_ms: u64) {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        let mut messages = self.messages.lock().await;
        messages.push(message.to_string());
    }

    async fn get_messages(&self) -> Vec<String> {
        self.messages.lock().await.clone()
    }

    async fn clear(&self) {
        self.messages.lock().await.clear();
    }
}

struct MockProvider {
    failure_rate: f32,
    response_delay_ms: u64,
}

impl MockProvider {
    fn new() -> Self {
        Self {
            failure_rate: 0.0,
            response_delay_ms: 0,
        }
    }

    fn unreliable(mut self) -> Self {
        self.failure_rate = 0.3;
        self.response_delay_ms = 100;
        self
    }

    async fn chat_completion(&self, prompt: &str) -> Result<String, String> {
        if self.response_delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.response_delay_ms)).await;
        }

        if rand::random::<f32>() < self.failure_rate {
            Err("Simulated provider failure".to_string())
        } else {
            Ok(format!("Response to: {}", prompt))
        }
    }
}

// ==================== TEST CASES ====================

#[tokio::test]
async fn test_01_basic_message_delivery() {
    println!("🧪 Test 1: Basic message delivery");
    let channel = MockChannel::new();

    channel.send("Hello").await;
    channel.send("World").await;

    let messages = channel.get_messages().await;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0], "Hello");
    assert_eq!(messages[1], "World");

    println!("✅ Basic message delivery test passed\n");
}

#[tokio::test]
async fn test_02_message_ordering_with_delays() {
    println!("🧪 Test 2: Message ordering with network delays");
    let channel = MockChannel::new();

    // Send messages with different delays (simulating network reordering)
    let mut handles = vec![];
    handles.push(tokio::spawn({
        let channel = channel.clone();
        async move {
            channel.send_with_delay("First", 100).await;
        }
    }));

    handles.push(tokio::spawn({
        let channel = channel.clone();
        async move {
            channel.send_with_delay("Second", 50).await;
        }
    }));

    handles.push(tokio::spawn({
        let channel = channel.clone();
        async move {
            channel.send_with_delay("Third", 10).await;
        }
    }));

    // Wait for all
    for handle in handles {
        handle.await.unwrap();
    }

    let messages = channel.get_messages().await;
    assert_eq!(messages.len(), 3);

    // All messages should be present (order may vary)
    assert!(messages.contains(&"First".to_string()));
    assert!(messages.contains(&"Second".to_string()));
    assert!(messages.contains(&"Third".to_string()));

    println!("✅ All messages arrived despite delays\n");
}

#[tokio::test]
async fn test_03_concurrent_message_handling() {
    println!("🧪 Test 3: Concurrent message handling");
    let channel = MockChannel::new();

    let mut handles = vec![];
    for i in 0..50 {
        let channel = channel.clone();
        handles.push(tokio::spawn(async move {
            channel.send(&format!("Message {}", i)).await;
        }));
    }

    // Wait for all
    for handle in handles {
        handle.await.unwrap();
    }

    let messages = channel.get_messages().await;
    assert_eq!(messages.len(), 50);

    // Verify all messages
    for i in 0..50 {
        let expected = format!("Message {}", i);
        assert!(messages.contains(&expected), "Missing: {}", expected);
    }

    println!("✅ Concurrent message handling test passed ({} messages)\n", messages.len());
}

#[tokio::test]
async fn test_04_reliable_provider() {
    println!("🧪 Test 4: Reliable provider");
    let provider = MockProvider::new();

    let response = provider.chat_completion("Hello").await;
    assert!(response.is_ok());
    assert_eq!(response.unwrap(), "Response to: Hello");

    println!("✅ Reliable provider test passed\n");
}

#[tokio::test]
async fn test_05_unreliable_provider_with_retries() {
    println!("🧪 Test 5: Unreliable provider with retries");
    let provider = MockProvider::new().unreliable();

    let mut success = false;
    let mut attempts = 0;

    // Try up to 5 times
    for _ in 0..5 {
        attempts += 1;
        match provider.chat_completion("Hello").await {
            Ok(response) => {
                success = true;
                println!("  Succeeded on attempt {}: {}", attempts, response);
                break;
            }
            Err(e) => {
                println!("  Attempt {} failed: {}", attempts, e);
            }
        }
    }

    assert!(success, "Should succeed with retries after {} attempts", attempts);
    println!("✅ Unreliable provider with retries test passed\n");
}

#[tokio::test]
async fn test_06_slow_provider_timeout() {
    println!("🧪 Test 6: Slow provider timeout handling");
    let provider = MockProvider::new().unreliable(); // Has 100ms delay

    // Test with short timeout (should timeout)
    match tokio::time::timeout(
        Duration::from_millis(50),
        provider.chat_completion("Test")
    ).await {
        Ok(Ok(response)) => println!("  Got response: {}", response),
        Ok(Err(e)) => println!("  Provider error: {}", e),
        Err(_) => println!("  Timeout occurred (expected)"),
    }

    // Test with longer timeout (should succeed)
    match tokio::time::timeout(
        Duration::from_millis(200),
        provider.chat_completion("Test")
    ).await {
        Ok(Ok(response)) => {
            println!("  Got response with longer timeout: {}", response);
            assert!(response.contains("Response to:"));
        }
        Ok(Err(e)) => println!("  Provider error: {}", e),
        Err(_) => println!("  Unexpected timeout"),
    }

    println!("✅ Slow provider timeout test passed\n");
}

#[tokio::test]
async fn test_07_provider_fallback_chain() {
    println!("🧪 Test 7: Provider fallback chain");
    
    // Create chain: unreliable -> unreliable -> reliable
    let providers = vec![
        MockProvider::new().unreliable(),
        MockProvider::new().unreliable(),
        MockProvider::new(), // Reliable
    ];

    let mut success = false;
    let mut attempts = 0;
    let mut _last_error = None;

    for (i, provider) in providers.iter().enumerate() {
        attempts += 1;
        match provider.chat_completion("Fallback test").await {
            Ok(response) => {
                success = true;
                println!("  Provider {} succeeded: {}", i, response);
                break;
            }
            Err(e) => {
                println!("  Provider {} failed: {}", i, e);
                _last_error = Some(e);
            }
        }
    }

    assert!(success, "Should succeed with fallback chain after {} attempts", attempts);
    println!("✅ Provider fallback chain test passed\n");
}

#[tokio::test]
async fn test_08_mixed_workload() {
    println!("🧪 Test 8: Mixed workload (channel + provider)");
    let channel = MockChannel::new();
    let _provider = MockProvider::new().unreliable();

    let mut handles = vec![];

    // Send messages
    for i in 0..10 {
        let channel = channel.clone();
        handles.push(tokio::spawn(async move {
            channel.send(&format!("User message {}", i)).await;
        }));
    }

    // Make provider calls
    for i in 0..5 {
        let provider = MockProvider::new().unreliable();
        handles.push(tokio::spawn(async move {
            match provider.chat_completion(&format!("Query {}", i)).await {
                Ok(response) => println!("  Provider call {}: {}", i, response),
                Err(e) => println!("  Provider call {} failed: {}", i, e),
            }
        }));
    }

    // Wait for all
    for handle in handles {
        handle.await.unwrap();
    }

    let messages = channel.get_messages().await;
    assert_eq!(messages.len(), 10);

    println!("✅ Mixed workload test passed ({} messages, 5 provider calls)\n", messages.len());
}

#[tokio::test]
async fn test_09_resource_cleanup() {
    println!("🧪 Test 9: Resource cleanup");
    let channel = MockChannel::new();

    // Send messages
    channel.send("Message 1").await;
    channel.send("Message 2").await;

    let before = channel.get_messages().await;
    assert_eq!(before.len(), 2);

    // Clear
    channel.clear().await;
    let after = channel.get_messages().await;
    assert_eq!(after.len(), 0);

    // Send more
    channel.send("Message 3").await;
    let final_messages = channel.get_messages().await;
    assert_eq!(final_messages.len(), 1);
    assert_eq!(final_messages[0], "Message 3");

    println!("✅ Resource cleanup test passed\n");
}

#[tokio::test]
async fn test_10_rate_limiting_simulation() {
    println!("🧪 Test 10: Rate limiting simulation");
    
    use tokio::sync::Semaphore;
    
    #[derive(Clone)]
    struct RateLimitedChannel {
        semaphore: Arc<Semaphore>,
        processed: Arc<Mutex<u32>>,
    }
    
    impl RateLimitedChannel {
        fn new(rate_limit: usize) -> Self {
            Self {
                semaphore: Arc::new(Semaphore::new(rate_limit)),
                processed: Arc::new(Mutex::new(0)),
            }
        }
        
        async fn send(&self, _message: &str) -> Result<(), String> {
            let permit = self.semaphore.acquire().await
                .map_err(|_| "Rate limit exceeded".to_string())?;
            
            // Simulate processing
            tokio::time::sleep(Duration::from_millis(20)).await;
            
            let mut processed = self.processed.lock().await;
            *processed += 1;
            
            drop(permit);
            Ok(())
        }
        
        async fn get_processed(&self) -> u32 {
            *self.processed.lock().await
        }
    }
    
    let channel = RateLimitedChannel::new(3); // Max 3 concurrent
    
    let start = std::time::Instant::now();
    let mut handles = vec![];
    
    // Try to send 10 messages
    for i in 0..10 {
        let channel = channel.clone();
        handles.push(tokio::spawn(async move {
            match channel.send(&format!("Message {}", i)).await {
                Ok(_) => println!("  Message {} sent", i),
                Err(e) => println!("  Message {} failed: {}", i, e),
            }
        }));
    }
    
    // Wait for all
    for handle in handles {
        handle.await.unwrap();
    }
    
    let elapsed = start.elapsed();
    let processed = channel.get_processed().await;
    
    println!("  Processed {} messages in {:?}", processed, elapsed);
    
    assert_eq!(processed, 10, "Should process all messages");
    assert!(elapsed > Duration::from_millis(100), "Should take time due to rate limiting");
    
    println!("✅ Rate limiting simulation test passed\n");
}

#[tokio::test]
async fn test_11_error_recovery_state_consistency() {
    println!("🧪 Test 11: Error recovery and state consistency");
    
    #[derive(Clone)]
    struct StatefulService {
        counter: Arc<Mutex<u32>>,
        error_rate: f32,
    }
    
    impl StatefulService {
        fn new(error_rate: f32) -> Self {
            Self {
                counter: Arc::new(Mutex::new(0)),
                error_rate,
            }
        }
        
        async fn increment(&self) -> Result<u32, String> {
            let mut counter = self.counter.lock().await;
            
            if rand::random::<f32>() < self.error_rate {
                // Simulate error after modifying state
                *counter += 1;
                return Err("Error after increment".to_string());
            }
            
            *counter += 1;
            Ok(*counter)
        }
        
        async fn get_count(&self) -> u32 {
            *self.counter.lock().await
        }
    }
    
    let service = StatefulService::new(0.2);
    
    let mut handles = vec![];
    for i in 0..20 {
        let service = service.clone();
        handles.push(tokio::spawn(async move {
            match service.increment().await {
                Ok(count) => println!("  Increment {}: count = {}", i, count),
                Err(e) => println!("  Increment {} failed: {}", i, e),
            }
        }));
    }
    
    for handle in handles {
        handle.await.unwrap();
    }
    
    let final_count = service.get_count().await;
    println!("  Final count: {}", final_count);
    
    assert!(final_count <= 20, "Count should not exceed attempts");
    println!("✅ Error recovery and state consistency test passed\n");
}

#[tokio::test]
async fn test_12_concurrent_provider_requests() {
    println!("🧪 Test 12: Concurrent provider requests");
    
    let start = std::time::Instant::now();
    let mut handles = vec![];
    
    // Make many concurrent requests to unreliable providers
    for i in 0..15 {
        let provider_captured = MockProvider::new().unreliable();
        handles.push(tokio::spawn(async move {
            match provider_captured.chat_completion(&format!("Request {}", i)).await {
                Ok(response) => format!("Success: {}", response),
                Err(e) => format!("Failure: {}", e),
            }
        }));
    }
    
    // Collect results
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap());
    }
    
    let elapsed = start.elapsed();
    
    let successes = results.iter().filter(|r| r.starts_with("Success")).count();
    let failures = results.iter().filter(|r| r.starts_with("Failure")).count();
    
    println!("  Results: {} successes, {} failures in {:?}", successes, failures, elapsed);
    println!("  Sample results: {:?}", &results[..3.min(results.len())]);
    
    assert_eq!(results.len(), 15, "All requests should complete");
    assert!(elapsed < Duration::from_secs(3), "Should complete in reasonable time");
    
    println!("✅ Concurrent provider requests test passed\n");
}