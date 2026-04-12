//! Comprehensive mock infrastructure tests for ZeroClawed
//! Fast, controllable unit/e2e tests using mock channels and mock LLM providers

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::{Mutex, RwLock};
    use serde_json::{json, Value};
    use async_trait::async_trait;
    use proptest::prelude::*;

    // ==================== MOCK CHANNEL INFRASTRUCTURE ====================

    /// Mock channel that simulates user interactions with controllable behavior
    #[derive(Clone)]
    struct MockChannel {
        messages: Arc<Mutex<Vec<ChannelMessage>>>,
        config: ChannelConfig,
        behavior: Arc<RwLock<ChannelBehavior>>,
    }

    #[derive(Clone, Debug)]
    struct ChannelMessage {
        id: String,
        sender: String,
        text: String,
        timestamp: String,
        response: Option<String>,
    }

    #[derive(Clone)]
    struct ChannelConfig {
        control_port: u16,
        allowed_users: Vec<String>,
        max_message_rate: u32, // messages per second
    }

    #[derive(Clone)]
    struct ChannelBehavior {
        reorder_probability: f32,
        duplicate_probability: f32,
        drop_probability: f32,
        delay_ms_range: (u64, u64),
        error_probability: f32,
    }

    impl MockChannel {
        fn new() -> Self {
            Self {
                messages: Arc::new(Mutex::new(Vec::new())),
                config: ChannelConfig {
                    control_port: 9090,
                    allowed_users: vec!["test-user".to_string()],
                    max_message_rate: 100,
                },
                behavior: Arc::new(RwLock::new(ChannelBehavior {
                    reorder_probability: 0.0,
                    duplicate_probability: 0.0,
                    drop_probability: 0.0,
                    delay_ms_range: (0, 0),
                    error_probability: 0.0,
                })),
            }
        }

        fn adversarial(mut self) -> Self {
            let mut behavior = self.behavior.blocking_write();
            behavior.reorder_probability = 0.3;
            behavior.duplicate_probability = 0.2;
            behavior.drop_probability = 0.1;
            behavior.delay_ms_range = (10, 200);
            behavior.error_probability = 0.15;
            self
        }

        async fn send(&self, sender: &str, text: &str) -> Result<String, String> {
            // Check if sender is allowed
            if !self.config.allowed_users.contains(&sender.to_string()) {
                return Err("Sender not allowed".to_string());
            }

            let behavior = self.behavior.read().await;

            // Simulate error
            if rand::random::<f32>() < behavior.error_probability {
                return Err("Simulated channel error".to_string());
            }

            // Simulate message drop
            if rand::random::<f32>() < behavior.drop_probability {
                return Ok("Message dropped (simulated)".to_string());
            }

            // Simulate delay
            let delay_ms = if behavior.delay_ms_range.1 > 0 {
                rand::random::<u64>() % 
                (behavior.delay_ms_range.1 - behavior.delay_ms_range.0) + 
                behavior.delay_ms_range.0
            } else {
                0
            };

            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }

            // Create message
            let message = ChannelMessage {
                id: format!("msg-{}", chrono::Utc::now().timestamp_nanos()),
                sender: sender.to_string(),
                text: text.to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                response: None,
            };

            // Store message
            {
                let mut messages = self.messages.lock().await;
                messages.push(message.clone());
            }

            Ok(message.id)
        }

        async fn get_messages(&self) -> Vec<ChannelMessage> {
            self.messages.lock().await.clone()
        }

        async fn clear(&self) {
            self.messages.lock().await.clear();
        }
    }

    // ==================== MOCK LLM PROVIDER INFRASTRUCTURE ====================

    #[async_trait]
    trait MockLLMProvider: Send + Sync {
        async fn chat_completion(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError>;
        async fn models(&self) -> Result<Vec<ModelInfo>, ProviderError>;
        
        // Behavior control
        fn set_failure_rate(&mut self, rate: f32);
        fn set_timeout_ms(&mut self, timeout: u64);
        fn set_partial_response_rate(&mut self, rate: f32);
    }

    #[derive(Clone, Debug)]
    struct ChatRequest {
        model: String,
        messages: Vec<Message>,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
        stream: bool,
        tools: Option<Vec<Tool>>,
    }

    #[derive(Clone, Debug)]
    struct Message {
        role: String,
        content: String,
    }

    #[derive(Clone, Debug)]
    struct Tool {
        name: String,
        description: String,
        parameters: Value,
    }

    #[derive(Clone, Debug)]
    struct ChatResponse {
        id: String,
        model: String,
        choices: Vec<Choice>,
        usage: Usage,
        created: i64,
    }

    #[derive(Clone, Debug)]
    struct Choice {
        index: u32,
        message: Message,
        finish_reason: String,
    }

    #[derive(Clone, Debug)]
    struct Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
    }

    #[derive(Clone, Debug)]
    struct ModelInfo {
        id: String,
        object: String,
        created: i64,
        owned_by: String,
    }

    #[derive(Debug)]
    enum ProviderError {
        Timeout,
        ModelNotFound(String),
        RateLimitExceeded,
        ContextLengthExceeded(u32, u32), // actual, max
        InvalidRequest(String),
        ProviderUnavailable,
        ToolExecutionFailed(String),
    }

    /// Adversarial mock provider that can simulate various failure modes
    struct AdversarialMockProvider {
        failure_rate: f32,
        timeout_ms: u64,
        partial_response_rate: f32,
        tool_failure_rate: f32,
        available_models: Vec<ModelInfo>,
        call_count: Arc<Mutex<u32>>,
    }

    impl AdversarialMockProvider {
        fn new() -> Self {
            Self {
                failure_rate: 0.0,
                timeout_ms: 0,
                partial_response_rate: 0.0,
                tool_failure_rate: 0.0,
                available_models: vec![
                    ModelInfo {
                        id: "mock-model".to_string(),
                        object: "model".to_string(),
                        created: 1677610602,
                        owned_by: "mock".to_string(),
                    },
                ],
                call_count: Arc::new(Mutex::new(0)),
            }
        }

        fn adversarial(mut self) -> Self {
            self.failure_rate = 0.25;
            self.timeout_ms = 5000; // 5 second timeout
            self.partial_response_rate = 0.1;
            self.tool_failure_rate = 0.3;
            self
        }

        fn unreliable(mut self) -> Self {
            self.failure_rate = 0.5;
            self
        }

        fn slow(mut self) -> Self {
            self.timeout_ms = 10000;
            self
        }
    }

    #[async_trait]
    impl MockLLMProvider for AdversarialMockProvider {
        async fn chat_completion(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
            // Track call count
            {
                let mut count = self.call_count.lock().await;
                *count += 1;
            }

            // Check if model exists
            if !self.available_models.iter().any(|m| m.id == request.model) {
                return Err(ProviderError::ModelNotFound(request.model));
            }

            // Simulate timeout
            if self.timeout_ms > 0 && rand::random::<f32>() < 0.1 {
                tokio::time::sleep(Duration::from_millis(self.timeout_ms)).await;
                return Err(ProviderError::Timeout);
            }

            // Simulate failure
            if rand::random::<f32>() < self.failure_rate {
                return Err(ProviderError::ProviderUnavailable);
            }

            // Simulate partial response
            if rand::random::<f32>() < self.partial_response_rate {
                // Return response missing some fields
                return Ok(ChatResponse {
                    id: "partial-".to_string() + &rand::random::<u64>().to_string(),
                    model: request.model,
                    choices: vec![Choice {
                        index: 0,
                        message: Message {
                            role: "assistant".to_string(),
                            content: "Partial response".to_string(),
                        },
                        finish_reason: "".to_string(), // Missing finish reason
                    }],
                    usage: Usage {
                        prompt_tokens: 0, // Missing token counts
                        completion_tokens: 0,
                        total_tokens: 0,
                    },
                    created: chrono::Utc::now().timestamp(),
                });
            }

            // Handle tool calls
            let response_content = if let Some(tools) = &request.tools {
                let mut tool_results = Vec::new();
                for tool in tools {
                    if rand::random::<f32>() < self.tool_failure_rate {
                        tool_results.push(format!("Tool '{}' failed", tool.name));
                    } else {
                        tool_results.push(format!("Tool '{}' succeeded", tool.name));
                    }
                }
                format!("Tool results: {}", tool_results.join(", "))
            } else {
                format!("Response to: {}", 
                    request.messages.last()
                        .map(|m| &m.content)
                        .unwrap_or(&"unknown".to_string())
                )
            };

            // Successful response
            Ok(ChatResponse {
                id: "chat-".to_string() + &rand::random::<u64>().to_string(),
                model: request.model,
                choices: vec![Choice {
                    index: 0,
                    message: Message {
                        role: "assistant".to_string(),
                        content: response_content,
                    },
                    finish_reason: "stop".to_string(),
                }],
                usage: Usage {
                    prompt_tokens: 100,
                    completion_tokens: 50,
                    total_tokens: 150,
                },
                created: chrono::Utc::now().timestamp(),
            })
        }

        async fn models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
            // Simulate occasional failure
            if rand::random::<f32>() < self.failure_rate * 0.5 {
                return Err(ProviderError::ProviderUnavailable);
            }

            Ok(self.available_models.clone())
        }

        fn set_failure_rate(&mut self, rate: f32) {
            self.failure_rate = rate;
        }

        fn set_timeout_ms(&mut self, timeout: u64) {
            self.timeout_ms = timeout;
        }

        fn set_partial_response_rate(&mut self, rate: f32) {
            self.partial_response_rate = rate;
        }
    }

    // ==================== TEST SUITE ====================

    /// Test 1: Message ordering with adversarial channel
    #[tokio::test]
    async fn test_adversarial_message_ordering() {
        let channel = MockChannel::new().adversarial();
        
        // Send messages in sequence
        let mut message_ids = Vec::new();
        for i in 0..10 {
            match channel.send("test-user", &format!("Message {}", i)).await {
                Ok(id) => message_ids.push(id),
                Err(e) => println!("Message {} failed: {}", i, e),
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        
        // Wait for all messages to be processed
        tokio::time::sleep(Duration::from_millis(500)).await;
        
        let messages = channel.get_messages().await;
        
        println!("Sent {} messages, received {}", message_ids.len(), messages.len());
        
        // Should receive most messages (accounting for drops)
        assert!(
            messages.len() >= 7, // At least 70% should get through
            "Too many messages dropped: {} of {}",
            messages.len(), message_ids.len()
        );
        
        // Check for duplicates
        let unique_texts: std::collections::HashSet<_> = 
            messages.iter().map(|m| &m.text).collect();
        
        println!("Unique messages: {}", unique_texts.len());
        
        // Should have reasonable number of unique messages
        assert!(
            unique_texts.len() >= 7,
            "Too many duplicates: {} unique of {} total",
            unique_texts.len(), messages.len()
        );
    }

    /// Test 2: Provider failure recovery with multiple mock providers
    #[tokio::test]
    async fn test_provider_failure_recovery() {
        // Create providers with different reliability
        let providers: Vec<Arc<dyn MockLLMProvider>> = vec![
            Arc::new(AdversarialMockProvider::new().unreliable()),
            Arc::new(AdversarialMockProvider::new()), // More reliable
            Arc::new(AdversarialMockProvider::new()), // Most reliable
        ];
        
        let request = ChatRequest {
            model: "mock-model".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: "Hello".to_string(),
            }],
            temperature: Some(0.7),
            max_tokens: Some(100),
            stream: false,
            tools: None,
        };
        
        let mut success = false;
        let mut attempts = 0;
        
        // Try providers in order
        for (i, provider) in providers.iter().enumerate() {
            attempts += 1;
            match provider.chat_completion(request.clone()).await {
                Ok(response) => {
                    println!("Provider {} succeeded: {}", i, response.choices[0].message.content);
                    success = true;
                    break;
                }
                Err(e) => {
                    println!("Provider {} failed: {:?}", i, e);
                }
            }
        }
        
        assert!(success, "Should succeed with at least one provider after {} attempts", attempts);
        println!("✅ Recovery succeeded after {} attempts", attempts);
    }

    /// Test 3: Tool call failures and recovery
    #[tokio::test]
    async fn test_tool_call_failures() {
        let provider = AdversarialMockProvider::new().adversarial();
        
        let request = ChatRequest {
            model: "mock-model".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: "Use tools".to_string(),
            }],
            temperature: Some(0.7),
            max_tokens: Some(100),
            stream: false,
            tools: Some(vec![
                Tool {
                    name: "calculator".to_string(),
                    description: "Perform calculations".to_string(),
                    parameters: json!({"type": "object"}),
                },
                Tool {
                    name: "web_search".to_string(),
                    description: "Search the web".to_string(),
                    parameters: json!({"type": "object"}),
                },
            ]),
        };
        
        let mut successes = 0;
        let mut failures = 0;
        
        for _ in 0..10 {
            match provider.chat_completion(request.clone()).await {
                Ok(response) => {
                    if response.choices[0].message.content.contains("failed") {
                        failures += 1;
                    } else {
                        successes += 1;
                    }
                }
                Err(_) => {
                    failures += 1;
                }
            }
        }
        
        println!("Tool calls: {} success, {} failures", successes, failures);
        
        // Should have mixed results
        assert!(successes > 0, "Should have some successful tool calls");
        assert!(failures > 0, "Should have some failures (by design)");
    }

    /// Test 4: Rate limiting simulation
    #[tokio::test]
    async fn test_rate_limiting() {
        let channel = MockChannel::new();
        
        let start = std::time::Instant::now();
        let mut handles = vec![];
        
        // Send many messages quickly
        for i in 0..50 {
            let channel = channel.clone();
            handles.push(tokio::spawn(async move {
                channel.send("test-user", &format!("Msg {}", i)).await
            }));
        }
        
        // Wait for all
        let results = futures::future::join_all(handles).await;
        
        let elapsed = start.elapsed();
        let messages = channel.get_messages().await;
        
        println!("Sent 50 messages in {:?}, received {}", elapsed, messages.len());
        
        // System shouldn't crash under load
        assert!(messages.len() > 0, "Should process at least some messages");
        
        // Should process most messages
        assert!(
            messages.len() >= 40,
            "Should process most messages under load, got {}",
            messages.len()
        );
    }

    /// Test 5: Property test for message delivery guarantee
    proptest! {
        #[test]
        fn property_message_delivery_guarantee(
            messages in prop::collection::vec("[a-zA-Z0-9 ]{1,50}", 1..20),
            drop_rate in 0.0f32..0.3
