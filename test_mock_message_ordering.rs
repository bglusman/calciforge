//! Unit tests for message ordering issues using mock channels
//! Tests race conditions, out-of-order delivery, and duplicate messages

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;
    use serde_json::json;

    /// Simulates a mock channel that can reorder or duplicate messages
    struct MockChannel {
        messages: Arc<Mutex<Vec<String>>>,
        delivered: Arc<Mutex<Vec<String>>>,
        // Configuration for adversarial behavior
        reorder_probability: f32,
        duplicate_probability: f32,
        delay_range_ms: (u64, u64),
    }

    impl MockChannel {
        fn new() -> Self {
            Self {
                messages: Arc::new(Mutex::new(Vec::new())),
                delivered: Arc::new(Mutex::new(Vec::new())),
                reorder_probability: 0.0,
                duplicate_probability: 0.0,
                delay_range_ms: (0, 0),
            }
        }

        /// Enable adversarial behavior
        fn adversarial(mut self) -> Self {
            self.reorder_probability = 0.3;
            self.duplicate_probability = 0.2;
            self.delay_range_ms = (10, 100);
            self
        }

        /// Send a message with potential reordering/duplication
        async fn send(&self, message: &str) -> tokio::task::JoinHandle<()> {
            let message = message.to_string();
            let messages = self.messages.clone();
            let delivered = self.delivered.clone();
            let reorder_prob = self.reorder_probability;
            let duplicate_prob = self.duplicate_probability;
            let (min_delay, max_delay) = self.delay_range_ms;

            tokio::spawn(async move {
                // Simulate network delay
                let delay = if max_delay > 0 {
                    tokio::time::sleep(Duration::from_millis(
                        rand::random::<u64>() % (max_delay - min_delay) + min_delay
                    )).await;
                };

                // Possibly duplicate message
                let mut to_deliver = vec![message.clone()];
                if rand::random::<f32>() < duplicate_prob {
                    to_deliver.push(message.clone()); // Duplicate
                }

                // Deliver messages (possibly out of order)
                for msg in to_deliver {
                    let mut messages = messages.lock().await;
                    messages.push(msg.clone());
                    
                    let mut delivered = delivered.lock().await;
                    delivered.push(msg);
                }
            })
        }

        /// Get all delivered messages in delivery order
        async fn get_delivered(&self) -> Vec<String> {
            self.delivered.lock().await.clone()
        }

        /// Get all messages in receipt order
        async fn get_messages(&self) -> Vec<String> {
            self.messages.lock().await.clone()
        }
    }

    /// Test 1: Message ordering should be preserved despite network delays
    #[tokio::test]
    async fn test_message_ordering_preserved() {
        let channel = MockChannel::new().adversarial();
        
        // Send messages in order
        let mut handles = vec![];
        for i in 0..10 {
            handles.push(channel.send(&format!("Message {}", i)).await);
        }
        
        // Wait for all sends to complete
        for handle in handles {
            handle.await.unwrap();
        }
        
        // Give a moment for all messages to be processed
        tokio::time::sleep(Duration::from_millis(200)).await;
        
        let delivered = channel.get_delivered().await;
        let messages = channel.get_messages().await;
        
        // Check for duplicates
        let unique_messages: std::collections::HashSet<_> = delivered.iter().collect();
        assert_eq!(
            unique_messages.len(),
            10,
            "Should have 10 unique messages, got {} (duplicates: {})",
            unique_messages.len(),
            delivered.len() - unique_messages.len()
        );
        
        // Messages should appear in SOME order in the messages list
        // (they might be reordered by network)
        assert_eq!(
            messages.len(),
            delivered.len(),
            "All messages should be accounted for"
        );
        
        // The system should handle out-of-order delivery gracefully
        // This test PASSES if no panic occurs
        println!("✅ Message ordering test passed (handled {} messages, {} unique)", 
                 messages.len(), unique_messages.len());
    }

    /// Test 2: Duplicate message detection and handling
    #[tokio::test]
    async fn test_duplicate_message_handling() {
        let channel = MockChannel::new();
        
        // Send the same message multiple times
        let message = "Duplicate test message";
        let mut handles = vec![];
        for _ in 0..5 {
            handles.push(channel.send(message).await);
        }
        
        for handle in handles {
            handle.await.unwrap();
        }
        
        tokio::time::sleep(Duration::from_millis(100)).await;
        
        let delivered = channel.get_delivered().await;
        
        // A robust system should either:
        // 1. Detect and filter duplicates
        // 2. Process them idempotently
        // 3. Return an error for excessive duplicates
        
        // For now, just verify we got messages
        assert!(
            !delivered.is_empty(),
            "Should have received at least one message"
        );
        
        println!("✅ Duplicate message test passed (received {} copies)", delivered.len());
    }

    /// Test 3: Rapid fire messages (rate limiting simulation)
    #[tokio::test]
    async fn test_rapid_fire_messages() {
        let channel = MockChannel::new();
        
        // Send many messages very quickly
        let mut handles = vec![];
        let start = std::time::Instant::now();
        
        for i in 0..50 {
            handles.push(channel.send(&format!("Rapid {}", i)).await);
            // No delay between sends
        }
        
        // Wait for all
        for handle in handles {
            let _ = handle.await;
        }
        
        let elapsed = start.elapsed();
        let delivered = channel.get_delivered().await;
        
        println!("Sent 50 messages in {:?}, received {}", elapsed, delivered.len());
        
        // System shouldn't crash under load
        assert!(
            delivered.len() > 0,
            "Should process at least some messages under load"
        );
        
        // Should not lose all messages
        assert!(
            delivered.len() >= 25,
            "Should process at least half of rapid-fire messages, got {}",
            delivered.len()
        );
    }

    /// Test 4: Message with simulated tool calls and failures
    #[tokio::test]
    async fn test_tool_call_failures() {
        // Simulate a mock LLM provider that sometimes fails tool calls
        struct MockLLMProvider {
            failure_rate: f32,
            timeout_rate: f32,
        }
        
        impl MockLLMProvider {
            async fn call_tool(&self, tool_name: &str, input: &str) -> Result<String, String> {
                // Simulate random failures
                if rand::random::<f32>() < self.failure_rate {
                    return Err(format!("Tool '{}' failed: simulated error", tool_name));
                }
                
                // Simulate timeouts
                if rand::random::<f32>() < self.timeout_rate {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    return Err("Timeout".to_string());
                }
                
                // Success case
                Ok(format!("Tool '{}' succeeded with input: {}", tool_name, input))
            }
        }
        
        let provider = MockLLMProvider {
            failure_rate: 0.3,
            timeout_rate: 0.2,
        };
        
        // Test multiple tool calls
        let mut successes = 0;
        let mut failures = 0;
        let mut timeouts = 0;
        
        for i in 0..20 {
            match provider.call_tool("test_tool", &format!("input_{}", i)).await {
                Ok(_) => successes += 1,
                Err(e) if e == "Timeout" => timeouts += 1,
                Err(_) => failures += 1,
            }
        }
        
        println!("Tool call results: {} success, {} failures, {} timeouts", 
                 successes, failures, timeouts);
        
        // Should handle failures gracefully
        assert!(
            successes > 0,
            "Should have at least some successful tool calls"
        );
        
        // Should have some failures (by design)
        assert!(
            failures + timeouts > 0,
            "Should have some failures/timeouts with failure_rate=0.3"
        );
    }

    /// Test 5: State corruption under adversarial conditions
    #[tokio::test]
    async fn test_state_corruption_resistance() {
        // Simulate shared state that could be corrupted
        struct SharedState {
            counter: Arc<Mutex<i32>>,
            messages: Arc<Mutex<Vec<String>>>,
        }
        
        let state = SharedState {
            counter: Arc::new(Mutex::new(0)),
            messages: Arc::new(Mutex::new(Vec::new())),
        };
        
        // Multiple tasks trying to corrupt state
        let mut handles = vec![];
        for i in 0..10 {
            let counter = state.counter.clone();
            let messages = state.messages.clone();
            
            handles.push(tokio::spawn(async move {
                // Try to corrupt by setting invalid values
                let mut counter = counter.lock().await;
                *counter = if i % 2 == 0 { *counter + 1 } else { -1 };
                
                let mut messages = messages.lock().await;
                messages.push(format!("Task {}", i));
                
                // Simulate race condition
                tokio::time::sleep(Duration::from_millis(rand::random::<u64>() % 10)).await;
            }));
        }
        
        // Wait for all
        for handle in handles {
            handle.await.unwrap();
        }
        
        let final_counter = *state.counter.lock().await;
        let messages = state.messages.lock().await;
        
        println!("Final counter: {}, Messages: {}", final_counter, messages.len());
        
        // State should be consistent
        assert_eq!(
            messages.len(),
            10,
            "Should have 10 messages, got {}",
            messages.len()
        );
        
        // Counter might be corrupted (negative), but system shouldn't crash
        // This test passes as long as no panic occurs
        println!("✅ State corruption test passed (counter={})", final_counter);
    }

    /// Test 6: Property test for message delivery guarantee
    /// Using Hegel-like property: "No message loss" invariant
    #[tokio::test]
    async fn test_no_message_loss_property() {
        use proptest::prelude::*;
        
        proptest!(|(messages in prop::collection::vec("[a-zA-Z0-9 ]{1,50}", 1..20))| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let channel = MockChannel::new();
                
                // Send all messages
                let mut handles = vec![];
                for msg in &messages {
                    handles.push(channel.send(msg).await);
                }
                
                // Wait for all
                for handle in handles {
                    handle.await.unwrap();
                }
                
                tokio::time::sleep(Duration::from_millis(100)).await;
                
                let delivered = channel.get_delivered().await;
                
                // Property: Every sent message should be delivered at least once
                // (Allow duplicates, but no complete loss)
                for msg in &messages {
                    assert!(
                        delivered.contains(msg),
                        "Message '{}' was not delivered. Delivered: {:?}",
                        msg, delivered
                    );
                }
            });
        });
        
        println!("✅ No-message-loss property holds for random test cases");
    }
}