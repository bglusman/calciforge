//! Adversarial tests for ZeroClawed's model routing and fallback system
//! Tests race conditions, fallback loops, and resource exhaustion

#[cfg(test)]
mod tests {
    use reqwest;
    use serde_json::{json, Value};
    use std::time::Duration;
    use tokio::time;

    const ZEROCLAWED_URL: &str = "http://localhost:8083";
    const TEST_API_KEY: &str = "test-key-123";

    /// Test 1: Fallback loop attack
    /// Create a situation where fallbacks keep failing, causing infinite retries
    #[tokio::test]
    async fn test_fallback_loop_attack() {
        let client = reqwest::Client::new();
        
        // Configuration that might cause infinite fallback:
        // Primary model fails → fallback1 fails → fallback2 fails → back to primary?
        let payload = json!({
            "model": "always-fails-model",  // Non-existent model
            "messages": [{"role": "user", "content": "test"}],
            // Try to trigger max fallback depth
            "max_retries": 100
        });

        let start_time = time::Instant::now();
        
        let response = client
            .post(format!("{}/v1/chat/completions", ZEROCLAWED_URL))
            .header("Authorization", format!("Bearer {}", TEST_API_KEY))
            .header("Content-Type", "application/json")
            .json(&payload)
            .timeout(Duration::from_secs(30))  // Longer timeout for loop detection
            .send()
            .await;

        let elapsed = start_time.elapsed();
        
        match response {
            Ok(resp) => {
                let status = resp.status();
                
                // Critical: Should NOT succeed after many retries
                // Should fail fast with appropriate error
                if status.is_success() {
                    println!("⚠️ Request succeeded after {:?} - might indicate fallback loop", elapsed);
                } else {
                    // Failure is expected for non-existent model
                    println!("✅ Request failed as expected after {:?}", elapsed);
                }
                
                // Check that it didn't take too long (indicates loop)
                assert!(
                    elapsed < Duration::from_secs(10),
                    "FALLBACK LOOP VULNERABILITY: Request took {:?} - possible infinite retry loop",
                    elapsed
                );
            }
            Err(e) if e.is_timeout() => {
                // Timeout after 30s - STRONG INDICATION OF LOOP
                panic!("FALLBACK LOOP CONFIRMED: Request timed out after 30s - infinite retry loop detected");
            }
            Err(_) => {
                // Other errors are acceptable
            }
        }
    }

    /// Test 2: Race condition in concurrent model requests
    /// Same user requesting different models simultaneously
    #[tokio::test]
    async fn test_concurrent_model_race() {
        let client = reqwest::Client::new();
        
        // Send many concurrent requests with same API key
        // This might expose:
        // - Rate limiting bypass
        // - Cost tracking race conditions
        // - Model allocation conflicts
        
        let mut tasks = vec![];
        let request_count = 20;
        
        for i in 0..request_count {
            let client_clone = client.clone();
            let task = tokio::spawn(async move {
                let payload = json!({
                    "model": if i % 2 == 0 { "deepseek-chat" } else { "deepseek-reasoner" },
                    "messages": [{"role": "user", "content": format!("Request {}", i)}]
                });
                
                client_clone
                    .post(format!("{}/v1/chat/completions", ZEROCLAWED_URL))
                    .header("Authorization", format!("Bearer {}", TEST_API_KEY))
                    .header("Content-Type", "application/json")
                    .json(&payload)
                    .timeout(Duration::from_secs(10))
                    .send()
                    .await
            });
            tasks.push(task);
        }
        
        // Collect results
        let mut successes = 0;
        let mut failures = 0;
        let mut rate_limited = 0;
        
        for task in tasks {
            match task.await {
                Ok(Ok(resp)) => {
                    if resp.status().is_success() {
                        successes += 1;
                    } else if resp.status().as_u16() == 429 {
                        rate_limited += 1;
                    } else {
                        failures += 1;
                    }
                }
                Ok(Err(_)) => failures += 1,
                Err(_) => failures += 1,
            }
        }
        
        println!("Concurrent requests: {} success, {} rate limited, {} failed", 
                 successes, rate_limited, failures);
        
        // Critical assertions:
        // 1. Should have SOME rate limiting (not all 20 succeed)
        // 2. Should not crash or hang
        assert!(
            rate_limited > 0 || successes < request_count,
            "RATE LIMITING BYPASS: All {} concurrent requests succeeded - no rate limiting",
            request_count
        );
        
        assert!(
            successes + rate_limited + failures == request_count,
            "REQUEST LOSS: {} requests unaccounted for",
            request_count - (successes + rate_limited + failures)
        );
    }

    /// Test 3: Resource exhaustion via model switching
    /// Rapidly switch between models to exhaust connection pools
    #[tokio::test]
    async fn test_model_switching_exhaustion() {
        let client = reqwest::Client::new();
        
        // Models to cycle through (if ZeroClawed supports them)
        let models = vec!["deepseek-chat", "deepseek-reasoner", "gemma-4-31b-it"];
        
        let mut tasks = vec![];
        
        // Create burst of requests cycling through models
        for i in 0..50 {
            let client_clone = client.clone();
            let model = models[i % models.len()];
            
            let task = tokio::spawn(async move {
                let payload = json!({
                    "model": model,
                    "messages": [{"role": "user", "content": format!("Cycle {}", i)}],
                    "max_tokens": 10  // Small to reduce load
                });
                
                client_clone
                    .post(format!("{}/v1/chat/completions", ZEROCLAWED_URL))
                    .header("Authorization", format!("Bearer {}", TEST_API_KEY))
                    .header("Content-Type", "application/json")
                    .json(&payload)
                    .timeout(Duration::from_secs(5))
                    .send()
                    .await
            });
            tasks.push(task);
            
            // Small delay to spread requests but still create load
            time::sleep(Duration::from_millis(50)).await;
        }
        
        // Check system doesn't crash
        let mut completed = 0;
        for task in tasks {
            match task.await {
                Ok(_) => completed += 1,
                Err(_) => {} // Task join error
            }
        }
        
        println!("Completed {}/50 model switching requests", completed);
        
        // Should complete most requests (not crash)
        assert!(
            completed > 40,
            "RESOURCE EXHAUSTION: Only {}/50 requests completed - system may have crashed under load",
            completed
        );
    }

    /// Test 4: Invalid model parameters causing crashes
    #[tokio::test]
    async fn test_invalid_model_parameters() {
        let client = reqwest::Client::new();
        
        // Invalid parameter combinations that might crash
        let invalid_combos = vec![
            json!({
                "model": "deepseek-chat",
                "messages": [{"role": "user", "content": "test"}],
                "temperature": -1.0  // Invalid: negative temperature
            }),
            json!({
                "model": "deepseek-chat",
                "messages": [{"role": "user", "content": "test"}],
                "temperature": 2.0  // Invalid: >1.0
            }),
            json!({
                "model": "deepseek-chat",
                "messages": [{"role": "user", "content": "test"}],
                "max_tokens": 0  // Invalid: zero tokens
            }),
            json!({
                "model": "deepseek-chat",
                "messages": [{"role": "user", "content": "test"}],
                "max_tokens": 1000000  // Invalid: too large
            }),
            json!({
                "model": "deepseek-chat",
                "messages": [{"role": "user", "content": "test"}],
                "top_p": -0.5  // Invalid: negative
            }),
            json!({
                "model": "deepseek-chat",
                "messages": [{"role": "user", "content": "test"}],
                "top_p": 1.5  // Invalid: >1.0
            }),
        ];

        for (i, payload) in invalid_combos.iter().enumerate() {
            let response = client
                .post(format!("{}/v1/chat/completions", ZEROCLAWED_URL))
                .header("Authorization", format!("Bearer {}", TEST_API_KEY))
                .header("Content-Type", "application/json")
                .json(payload)
                .timeout(Duration::from_secs(5))
                .send()
                .await;

            match response {
                Ok(resp) => {
                    let status = resp.status();
                    
                    // Invalid parameters should get 400 Bad Request
                    // NOT 200 OK (which might indicate validation bypass)
                    // NOT 500 Internal Server Error (which would be a crash)
                    
                    if status.is_server_error() {
                        panic!("CRASH ON INVALID PARAMS: Got 500 for invalid parameters combo {}", i);
                    }
                    
                    if status.is_success() {
                        println!("⚠️ Invalid parameters accepted with 200 OK for combo {}", i);
                        // This might be OK if the provider handles validation,
                        // but ZeroClawed should validate
                    }
                }
                Err(e) if e.is_timeout() => {
                    // Timeout might indicate hang/crash
                    println!("⚠️ Timeout on invalid parameters combo {}", i);
                }
                Err(_) => {
                    // Connection error is acceptable
                }
            }
        }
    }

    /// Test 5: Model metadata manipulation
    /// Try to bypass model restrictions via metadata tricks
    #[tokio::test]
    async fn test_model_metadata_manipulation() {
        let client = reqwest::Client::new();
        
        // Try to trick the system with model metadata
        let trick_attempts = vec![
            // 1. Model name with special characters
            json!({
                "model": "deepseek-chat/v1",
                "messages": [{"role": "user", "content": "test"}]
            }),
            // 2. Model name with spaces
            json!({
                "model": "deepseek chat",
                "messages": [{"role": "user", "content": "test"}]
            }),
            // 3. Model name that's a subset of real model
            json!({
                "model": "deepseek",  // Missing -chat suffix
                "messages": [{"role": "user", "content": "test"}]
            }),
            // 4. Model name with extra prefix
            json!({
                "model": "zeroclawed/deepseek-chat",  // Adding provider prefix
                "messages": [{"role": "user", "content": "test"}]
            }),
            // 5. Case variation
            json!({
                "model": "DEEPSEEK-CHAT",
                "messages": [{"role": "user", "content": "test"}]
            }),
        ];

        for (i, payload) in trick_attempts.iter().enumerate() {
            let response = client
                .post(format!("{}/v1/chat/completions", ZEROCLAWED_URL))
                .header("Authorization", format!("Bearer {}", TEST_API_KEY))
                .header("Content-Type", "application/json")
                .json(payload)
                .timeout(Duration::from_secs(5))
                .send()
                .await;

            match response {
                Ok(resp) => {
                    let status = resp.status();
                    
                    // These should generally fail (model not found)
                    // Success might indicate fuzzy matching vulnerability
                    
                    if status.is_success() {
                        let body: Value = resp.json().await.unwrap_or_default();
                        let model_used = body.get("model")
                            .and_then(|m| m.as_str())
                            .unwrap_or("");
                        
                        println!("⚠️ Model trick {} succeeded. Model used: {}", i, model_used);
                        
                        // Check if it used the correct model despite trick
                        if !model_used.contains("deepseek-chat") {
                            println!("  ⚠️  Model mismatch - possible routing issue");
                        }
                    }
                }
                Err(_) => {
                    // Connection error is acceptable
                }
            }
        }
    }
}