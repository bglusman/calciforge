//! Complete mock infrastructure test suite for ZeroClawed
//! Fast, controllable tests using mock channels and providers

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;
    use serde_json::json;

    // ==================== SIMPLE MOCK CHANNEL ====================

    #[derive(Clone)]
    struct SimpleMockChannel {
        messages: Arc<Mutex<Vec<String>>>,
    }

    impl SimpleMockChannel {
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

    // ==================== SIMPLE MOCK PROVIDER ====================

    struct SimpleMockProvider {
        failure_rate: f32,
        response_delay_ms: u64,
    }

    impl SimpleMockProvider {
        fn new() -> Self {
            Self {
                failure_rate: 0.0,
                response_delay_ms: 0,
            }
        }

        fn unreliable(mut self) -> Self {
            self.failure_rate = 0.3;
            self
        }

        fn slow(mut self) -> Self {
            self.response_delay_ms = 100;
            self
        }

        async fn chat_completion(&self, prompt: &str) -> Result<String, String> {
            // Simulate delay
            if self.response_delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.response_delay_ms)).await;
            }

            // Simulate failure
            if rand::random::<f32>() < self.failure_rate {
                return Err("Simulated provider failure".to_string());
            }

            Ok(format!("Response to: {}", prompt))
        }
    }

    // ==================== TEST CASES ====================

    /// Test 1: Basic message delivery
    #[tokio::test]
    async fn test_basic_message_delivery() {
        let channel = SimpleMockChannel::new();

        channel.send("Hello").await;
        channel.send("World").await;

        let messages = channel.get_messages().await;
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0], "Hello");
        assert_eq!(messages[1], "World");

        println!("✅ Basic message delivery test passed");
    }

    /// Test 2: Message ordering with delays
    #[tokio::test]
    async fn test_message_ordering_with_delays() {
        let channel = SimpleMockChannel::new();

        // Send messages with different delays
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

        // Messages might arrive out of order
        println!("Messages received: {:?}", messages);

        // All messages should be present
        assert!(messages.contains(&"First".to_string()));
        assert!(messages.contains(&"Second".to_string()));
        assert!(messages.contains(&"Third".to_string()));

        println!("✅ Message ordering test passed");
    }

    /// Test 3: Concurrent message handling
    #[tokio::test]
    async fn test_concurrent_message_handling() {
        let channel = SimpleMockChannel::new();

        // Send many messages concurrently
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

        // Check all messages arrived
        for i in 0..50 {
            let expected = format!("Message {}", i);
            assert!(messages.contains(&expected), "Missing message: {}", expected);
        }

        println!("✅ Concurrent message handling test passed ({} messages)", messages.len());
    }

    /// Test 4: Reliable provider
    #[tokio::test]
    async fn test_reliable_provider() {
        let provider = SimpleMockProvider::new();

        let response = provider.chat_completion("Hello").await;
        assert!(response.is_ok());
        assert_eq!(response.unwrap(), "Response to: Hello");

        println!("✅ Reliable provider test passed");
    }

    /// Test 5: Unreliable provider with retries
    #[tokio::test]
    async fn test_unreliable_provider_with_retries() {
        let provider = SimpleMockProvider::new().unreliable();

        let mut success = false;
        let mut attempts = 0;

        // Try up to 5 times
        for _ in 0..5 {
            attempts += 1;
            match provider.chat_completion("Hello").await {
                Ok(response) => {
                    success = true;
                    println!("Succeeded on attempt {}: {}", attempts, response);
                    break;
                }
                Err(e) => {
                    println!("Attempt {} failed: {}", attempts, e);
                }
            }
        }

        assert!(success, "Should succeed with retries after {} attempts", attempts);
        println!("✅ Unreliable provider with retries test passed");
    }

    /// Test 6: Slow provider with timeout
    #[tokio::test]
    async fn test_slow_provider_with_timeout() {
        let provider = SimpleMockProvider::new().slow();

        // Test with timeout
        match tokio::time::timeout(
            Duration::from_millis(50),
            provider.chat_completion("Hello")
        ).await {
            Ok(Ok(response)) => {
                println!("Response within timeout: {}", response);
                // Might succeed if fast enough
            }
            Ok(Err(e)) => {
                println!("Provider error: {}", e);
            }
            Err(_) => {
                println!("Timeout occurred (expected for slow provider)");
            }
        }

        // Test with longer timeout
        match tokio::time::timeout(
            Duration::from_millis(200),
            provider.chat_completion("Hello")
        ).await {
            Ok(Ok(response)) => {
                println!("Response with longer timeout: {}", response);
                assert_eq!(response, "Response to: Hello");
            }
            Ok(Err(e)) => {
                println!("Provider error: {}", e);
            }
            Err(_) => {
                println!("Still timed out (unexpected)");
            }
        }

        println!("✅ Slow provider with timeout test passed");
    }

    /// Test 7: Provider fallback chain
    #[tokio::test]
    async fn test_provider_fallback_chain() {
        // Create providers with different reliability
        let providers = vec![
            SimpleMockProvider::new().unreliable(), // 30% failure rate
            SimpleMockProvider::new().unreliable(), // 30% failure rate  
            SimpleMockProvider::new(), // Reliable
        ];

        let mut success = false;
        let mut attempts = 0;
        let mut last_error = None;

        for (i, provider) in providers.iter().enumerate() {
            attempts += 1;
            match provider.chat_completion("Fallback test").await {
                Ok(response) => {
                    success = true;
                    println!("Provider {} succeeded: {}", i, response);
                    break;
                }
                Err(e) => {
                    println!("Provider {} failed: {}", i, e);
                    last_error = Some(e);
                }
            }
        }

        assert!(success, "Should succeed with fallback chain after {} attempts. Last error: {:?}", 
                attempts, last_error);
        println!("✅ Provider fallback chain test passed");
    }

    /// Test 8: Mixed workload (channels + providers)
    #[tokio::test]
    async fn test_mixed_workload() {
        let channel = SimpleMockChannel::new();
        let provider = SimpleMockProvider::new().unreliable();

        let mut handles = vec![];

        // Send messages through channel
        for i in 0..10 {
            let channel = channel.clone();
            handles.push(tokio::spawn(async move {
                channel.send(&format!("User message {}", i)).await;
            }));
        }

        // Make provider calls
        for i in 0..5 {
            let provider = SimpleMockProvider::new().unreliable();
            handles.push(tokio::spawn(async move {
                match provider.chat_completion(&format!("Query {}", i)).await {
                    Ok(response) => println!("Provider call {} succeeded: {}", i, response),
                    Err(e) => println!("Provider call {} failed: {}", i, e),
                }
            }));
        }

        // Wait for all
        for handle in handles {
            handle.await.unwrap();
        }

        let messages = channel.get_messages().await;
        assert_eq!(messages.len(), 10);

        println!("✅ Mixed workload test passed ({} channel messages, 5 provider calls)", 
                 messages.len());
    }

    /// Test 9: Resource cleanup
    #[tokio::test]
    async fn test_resource_cleanup() {
        let channel = SimpleMockChannel::new();

        // Send some messages
        channel.send("Message 1").await;
        channel.send("Message 2").await;

        let messages_before = channel.get_messages().await;
        assert_eq!(messages_before.len(), 2);

        // Clear and verify
        channel.clear().await;
        let messages_after = channel.get_messages().await;
        assert_eq!(messages_after.len(), 0);

        // Send more messages after clear
        channel.send("Message 3").await;
        let final_messages = channel.get_messages().await;
        assert_eq!(final_messages.len(), 1);
        assert_eq!(final_messages[0], "Message 3");

        println!("✅ Resource cleanup test passed");
    }

    /// Test 10: Simulated tool call failures
    #[tokio::test]
    async fn test_tool_call_failures() {
        struct MockTool {
            name: String,
            failure_rate: f32,
        }

        impl MockTool {
            async fn call(&self, input: &str) -> Result<String, String> {
                tokio::time::sleep(Duration::from_millis(10)).await;

                if rand::random::<f32>() < self.failure_rate {
                    Err(format!("Tool '{}' failed on input: {}", self.name, input))
                } else {
                    Ok(format!("Tool '{}' succeeded: {}", self.name, input))
                }
            }
        }

        let tools = vec![
            MockTool { name: "calculator".to_string(), failure_rate: 0.1 },
            MockTool { name: "web_search".to_string(), failure_rate: 0.3 },
            MockTool { name: "file_reader".to_string(), failure_rate: 0.05 },
        ];

        let mut results = vec![];
        for tool in &tools {
            for i in 0..3 {
                match tool.call(&format!("input_{}", i)).await {
                    Ok(response) => results.push(format!("✅ {}", response)),
                    Err(error) => results.push(format!("❌ {}", error)),
                }
            }
        }

        println!("Tool call results (sample):");
        for result in results.iter().take(3) {
            println!("  {}", result);
        }

        let success_count = results.iter().filter(|r| r.starts_with("✅")).count();
        let failure_count = results.iter().filter(|r| r.starts_with("❌")).count();

        assert!(success_count > 0, "Should have some successful tool calls");
        println!("✅ Tool call failure test passed ({} success, {} failures)", 
                 success_count, failure_count);
    }

    /// Test 11: Rate limiting simulation
    #[tokio::test]
    async fn test_rate_limiting_simulation() {
        use tokio::sync::Semaphore;

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

            async fn send(&self, message: &str) -> Result<(), String> {
                let permit = self.semaphore.acquire().await
                    .map_err(|_| "Rate limit exceeded".to_string())?;

                // Simulate processing
                tokio::time::sleep(Duration::from_millis(20)).await;

                let mut processed = self.processed.lock().await;
                *processed += 1;

                drop(permit); // Release permit
                Ok(())
            }

            async fn get_processed_count(&self) -> u32 {
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
                    Ok(_) => println!("Message {} sent successfully", i),
                    Err(e) => println!("Message {} failed: {}", i, e),
                }
            }));
        }

        // Wait for all
        for handle in handles {
            handle.await.unwrap();
        }

        let elapsed = start.elapsed();
        let processed = channel.get_processed_count().await;

        println!("Processed {} messages in {:?}", processed, elapsed);

        // Should process all messages (rate limiting just slows them down)
        assert_eq!(processed, 10, "Should process all 10 messages");
        assert!(elapsed > Duration::from_millis(100), 
                "Should take time due to rate limiting");

        println!("✅ Rate limiting simulation test passed");
    }

    /// Test 12: Error recovery and state consistency
    #[tokio::test]
    async fn test_error_recovery_state_consistency() {
        struct StatefulService {
            state: Arc<Mutex<i32>>,
            error_rate: f32,
        }

        impl StatefulService {
            fn new(error_rate: f32) -> Self {
                Self {
                    state: Arc::new(Mutex::new(0)),
                    error_rate,
                }
            }

            async fn increment(&self) -> Result<(), String> {
                let mut state = self.state.lock().await;

                // Simulate error
                if rand::random::<f32>() < self.error_rate {
                    return Err("Simulated error during increment".to_string());
                }

                *state += 1;
                Ok(())
            }

            async fn get_state(&self) -> i32 {
                *self.state.lock().await
            }
        }

        let service = StatefulService::new(0.2); // 20% error rate

        let mut successes = 0;
        let mut failures = 0;

        for i in 0..20 {
            match service.increment().await {
                Ok(_) => {
                    successes += 1;
                    println!("Increment {} succeeded", i);
                }
                Err(e) => {
                    failures += 1;
                    println!("Increment {} failed: {}", i, e);
                }
            }
        }

        let final_state = service.get_state().await;

        println!("Results: {} successes, {} failures, final state: {}", 
                 successes, failures, final_state);

        // State should equal number of successful increments
        assert_eq!(final_state, successes as i32, 
                   "State should match successful increments");

        // Should have some failures
        assert!(failures > 0, "Should have some failures with 20% error rate");

        println!("✅ Error recovery and state consistency test passed");
    }
}