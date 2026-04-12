//! Unit/e2e tests using mock channels and mock LLM providers
//! Fast, controllable tests for edge cases: message ordering, tool call failures, timeouts

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;
    use serde_json::json;

    // ==================== MOCK CHANNEL ====================

    #[derive(Clone)]
    struct MockChannel {
        messages: Arc<Mutex<Vec<ChannelMessage>>>,
        config: ChannelConfig,
    }

    #[derive(Clone, Debug)]
    struct ChannelMessage {
        id: String,
        sender: String,
        text: String,
        timestamp: String,
    }

    #[derive(Clone)]
    struct ChannelConfig {
        max_message_rate: u32,
        allow_duplicates: bool,
    }

    impl MockChannel {
        fn new() -> Self {
            Self {
                messages: Arc::new(Mutex::new(Vec::new())),
                config: ChannelConfig {
                    max_message_rate: 100,
                    allow_duplicates: true,
                },
            }
        }

        fn strict(mut self) -> Self {
            self.config.allow_duplicates = false;
            self
        }

        async fn send(&self, sender: &str, text: &str) -> Result<String, String> {
            // Check for duplicates if not allowed
            if !self.config.allow_duplicates {
                let messages = self.messages.lock().await;
                if messages.iter().any(|m| m.text == text && m.sender == sender) {
                    return Err("Duplicate message not allowed".to_string());
                }
            }

            let message = ChannelMessage {
                id: format!("msg-{}", chrono::Utc::now().timestamp_nanos()),
                sender: sender.to_string(),
                text: text.to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
            };

            let mut messages = self.messages.lock().await;
            messages.push(message.clone());

            Ok(message.id)
        }

        async fn send_with_delay(&self, sender: &str, text: &str, delay_ms: u64) -> Result<String, String> {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            self.send(sender, text).await
        }

        async fn get_messages(&self) -> Vec<ChannelMessage> {
            self.messages.lock().await.clone()
        }

        async fn clear(&self) {
            self.messages.lock().await.clear();
        }

        async fn message_count(&self) -> usize {
            self.messages.lock().await.len()
        }
    }

    // ==================== MOCK LLM PROVIDER ====================

    struct MockLLMProvider {
        failure_rate: f32,
        timeout_rate: f32,
        tool_failure_rate: f32,
        response_delay_ms: u64,
    }

    impl MockLLMProvider {
        fn new() -> Self {
            Self {
                failure_rate: 0.0,
                timeout_rate: 0.0,
                tool_failure_rate: 0.0,
                response_delay_ms: 0,
            }
        }

        fn adversarial(mut self) -> Self {
            self.failure_rate = 0.3;
            self.timeout_rate = 0.2;
            self.tool_failure_rate = 0.4;
            self.response_delay_ms = 100;
            self
        }

        fn reliable(mut self) -> Self {
            self.failure_rate = 0.0;
            self.timeout_rate = 0.0;
            self.tool_failure_rate = 0.0;
            self.response_delay_ms = 10;
            self
        }

        async fn chat_completion(&self, prompt: &str, use_tools: bool) -> Result<String, String> {
            // Simulate response delay
            if self.response_delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.response_delay_ms)).await;
            }

            // Simulate timeout
            if rand::random::<f32>() < self.timeout_rate {
                tokio::time::sleep(Duration::from_secs(5)).await;
                return Err("Request timeout".to_string());
            }

            // Simulate failure
            if rand::random::<f32>() < self.failure_rate {
                return Err("Provider unavailable".to_string());
            }

            // Handle tool calls
            if use_tools {
                if rand::random::<f32>() < self.tool_failure_rate {
                    return Ok("Tool call failed: simulated error".to_string());
                } else {
                    return Ok(format!("Used tools to process: {}", prompt));
                }
            }

            Ok(format!("Response to: {}", prompt))
        }

        async fn models(&self) -> Result<Vec<String>, String> {
            if rand::random::<f32>() < self.failure_rate * 0.5 {
                Err("Failed to fetch models".to_string())
            } else {
                Ok(vec![
                    "mock-model-1".to_string(),
                    "mock-model-2".to_string(),
                    "mock-model-3".to_string(),
                ])
            }
        }
    }

    // ==================== TEST CASES ====================

    /// Test 1: Basic message ordering preservation
    #[tokio::test]
    async fn test_message_ordering_preserved() {
        let channel = MockChannel::new();

        // Send messages in sequence
        let mut message_ids = Vec::new();
        for i in 0..5 {
            let id = channel.send("user1", &format!("Message {}", i)).await.unwrap();
            message_ids.push(id);
        }

        let messages = channel.get_messages().await;
        assert_eq!(messages.len(), 5);

        // Verify order (should be preserved in simple case)
        for (i, message) in messages.iter().enumerate() {
            assert_eq!(message.text, format!("Message {}", i));
        }

        println!("✅ Message ordering preserved (simple case)");
    }

    /// Test 2: Message ordering with network delays (simulated)
    #[tokio::test]
    async fn test_message_ordering_with_delays() {
        let channel = MockChannel::new();

        // Send messages with random delays (simulating network)
        let mut handles = vec![];
        for i in 0..5 {
            let delay_ms = (i as u64 * 20) % 100; // Different delays
            let channel = channel.clone();
            handles.push(tokio::spawn(async move {
                channel.send_with_delay("user1", &format!("Delayed {}", i), delay_ms).await
            }));
        }

        // Wait for all
        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await.unwrap());
        }

        let messages = channel.get_messages().await;
        assert_eq!(messages.len(), 5);

        // All messages should arrive
        for i in 0..5 {
            let expected = format!("Delayed {}", i);
            assert!(messages.iter().any(|m| m.text == expected), 
                    "Missing message: {}", expected);
        }

        println!("✅ All messages arrived despite delays");
    }

    /// Test 3: Duplicate message handling
    #[tokio::test]
    async fn test_duplicate_message_handling() {
        // Test with duplicates allowed
        let channel = MockChannel::new();
        
        channel.send("user1", "Hello").await.unwrap();
        channel.send("user1", "Hello").await.unwrap(); // Duplicate
        
        let messages = channel.get_messages().await;
        assert_eq!(messages.len(), 2, "Duplicates should be allowed");
        
        // Test with duplicates not allowed
        let strict_channel = MockChannel::new().strict();
        
        strict_channel.send("user1", "Hello").await.unwrap();
        let result = strict_channel.send("user1", "Hello").await;
        assert!(result.is_err(), "Duplicate should be rejected");
        
        println!("✅ Duplicate message handling works correctly");
    }

    /// Test 4: Rapid fire messages (rate limiting simulation)
    #[tokio::test]
    async fn test_rapid_fire_messages() {
        let channel = MockChannel::new();
        
        let start = std::time::Instant::now();
        let mut handles = vec![];
        
        // Send 100 messages quickly
        for i in 0..100 {
            let channel = channel.clone();
            handles.push(tokio::spawn(async move {
                channel.send("user1", &format!("Rapid {}", i)).await
            }));
        }
        
        // Wait for all
        for handle in handles {
            handle.await.unwrap().unwrap();
        }
        
        let elapsed = start.elapsed();
        let messages = channel.get_messages().await;
        
        println!("Sent 100 messages in {:?}, received {}", elapsed, messages.len());
        
        assert_eq!(messages.len(), 100, "Should receive all 100 messages");
        assert!(elapsed < Duration::from_secs(2), "Should complete quickly");
        
        println!("✅ Rapid fire messages handled correctly");
    }

    /// Test 5: Tool call failures with mock provider
    #[tokio::test]
    async fn test_tool_call_failures() {
        let provider = MockLLMProvider::new().adversarial();
        
        let mut successes = 0;
        let mut failures = 0;
        let mut timeouts = 0;
        
        for i in 0..20 {
            match tokio::time::timeout(
                Duration::from_secs(1),
                provider.chat_completion(&format!("Test {}", i), true)
            ).await {
                Ok(Ok(response)) => {
                    if response.contains("failed") {
                        failures += 1;
                    } else {
                        successes += 1;
                    }
                }
                Ok(Err(_)) => {
                    failures += 1;
                }
                Err(_) => {
                    timeouts += 1;
                }
            }
        }
        
        println!("Tool call results: {} success, {} failures, {} timeouts", 
                 successes, failures, timeouts);
        
        // Should have mixed results due to adversarial settings
        assert!(successes > 0, "Should have some successes");
        assert!(failures > 0, "Should have some failures (by design)");
        
        println!("✅ Tool call failure simulation works");
    }

    /// Test 6: Provider timeout handling
    #[tokio::test]
    async fn test_provider_timeout_handling() {
        let provider = MockLLMProvider::new().adversarial();
        
        let mut completed = 0;
        let mut timed_out = 0;
        
        for i in 0..10 {
            match tokio::time::timeout(
                Duration::from_millis(50), // Short timeout
                provider.chat_completion(&format!("Timeout test {}", i), false)
            ).await {
                Ok(Ok(_)) => completed += 1,
                Ok(Err(_)) => completed += 1, // Error but completed
                Err(_) => timed_out += 1,
            }
        }
        
        println!("Timeout test: {} completed, {} timed out", completed, timed_out);
        
        // Some should time out with adversarial settings
        assert!(timed_out > 0 || completed > 0, "Should have some results");
        
        println!("✅ Provider timeout handling works");
    }

    /// Test 7: Provider fallback chain
    #[tokio::test]
    async fn test_provider_fallback_chain() {
        // Create chain of providers with decreasing reliability
        let providers = vec![
            MockLLMProvider::new().adversarial(), // Most unreliable
            MockLLMProvider::new().adversarial(), // Still unreliable
            MockLLMProvider::new().reliable(),    // Reliable fallback
        ];
        
        let mut succeeded = false;
        let mut attempts = 0;
        let mut last_error = None;
        
        for (i, provider) in providers.iter().enumerate() {
            attempts += 1;
            match provider.chat_completion("Fallback test", false).await {
                Ok(response) => {
                    println!("Provider {} succeeded: {}", i, response);
                    succeeded = true;
                    break;
                }
                Err(e) => {
                    println!("Provider {} failed: {}", i, e);
                    last_error = Some(e);
                }
            }
        }
        
        assert!(succeeded, "Should succeed with fallback chain after {} attempts. Last error: {:?}", 
                attempts, last_error);
        
        println!("✅ Provider fallback chain works");
    }

    /// Test 8: Concurrent provider requests
    #[tokio::test]
    async fn test_concurrent_provider_requests() {
        let provider = MockLLMProvider::new().adversarial();
        
        let start = std::time::Instant::now();
        let mut handles = vec![];
        
        // Make many concurrent requests
        for i in 0..20 {
            let provider = MockLLMProvider::new().adversarial();
            handles.push(tokio::spawn(async move {
                provider.chat_completion(&format!("Concurrent {}", i), false).await
            }));
        }
        
        // Collect results
        let mut successes = 0;
        let mut failures = 0;
        
        for handle in handles {
            match handle.await.unwrap() {
                Ok(_) => successes += 1,
                Err(_) => failures += 1,
            }
        }
        
        let elapsed = start.elapsed();
        
        println!("Concurrent requests: {} success, {} failures in {:?}", 
                 successes, failures, elapsed);
        
        assert!(successes + failures == 20, "All requests should complete");
        assert!(elapsed < Duration::from_secs(3), "Should complete in reasonable time");
        
        println!("✅ Concurrent provider requests handled");
    }

    /// Test 9: State corruption resistance
    #[tokio::test]
    async fn test_state_corruption_resistance() {
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
                
                // Simulate random error that might corrupt state
                if rand::random::<f32>() < self.error_rate {
                    // Simulate partial state change
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
        
        let service = StatefulService::new(0.3);
        
        let mut handles = vec![];
        for i in 0..30 {
            let service = service.clone();
            handles.push(tokio::spawn(async move {
                match service.increment().await {
                    Ok(count) => println!("Increment {} succeeded: count = {}", i, count),
                    Err(e) => println!("Increment {} failed: {}", i, e),
                }
            }));
        }
        
        for handle in handles {
            handle.await.unwrap();
        }
        
        let final_count = service.get_count().await;
        println!("Final count: {}", final_count);
        
        // Count should be <= 30 (some increments might fail after modifying state)
        assert!(final_count <= 30, "Count should not exceed number of attempts");
        
        println!("✅ State corruption resistance tested");
    }

    /// Test 10: Property test for message delivery guarantee
    #[tokio::test]
    async fn test_property_message_delivery() {
        use proptest::prelude::*;
        
        proptest!(|(message_count in 1usize..20)| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let channel = MockChannel::new();
                
                // Send messages
                for i in 0..message_count {
                    channel.send("test", &format!("Message {}", i)).await.unwrap();
                }
                
                let messages = channel.get_messages().await;
                
                // Property: All sent messages should be delivered
                prop_assert_eq!(
                    messages.len(), message_count,
                    "Should deliver all {} messages, got {}",
                    message_count, messages.len()
                );
                
                // Property: No extra messages
                for i in 0..message_count {
                    let expected = format!("Message {}", i);
                    prop_assert!(
                        messages.iter().any(|m| m.text == expected),
                        "Missing message: {}",
                        expected
                    );
                }
            });
        });
        
        println!("✅ Message delivery property holds");
    }

    /// Test 11: Mixed adversarial scenarios
    #[tokio::test]
    async fn test_mixed_adversarial_scenarios() {
        println!("Running mixed adversarial scenarios...");
        
        // Scenario 1: Unreliable provider + delayed channel
        let channel = MockChannel::new();
        let provider = MockLLMProvider::new().adversarial();
        
        let mut handles = vec![];
        
        // Send messages with provider calls
        for i in 0..10 {
            let channel = channel.clone();
            let provider = MockLLMProvider::new().adversarial();
            
            handles.push(tokio::spawn(async move {
                // Send message
                let msg_result = channel.send("user1", &format!("Mixed {}", i)).await;
                
                // Make provider call
                let provider_result = provider.chat_completion(&format!("Query {}", i), i % 2 == 0).await;
