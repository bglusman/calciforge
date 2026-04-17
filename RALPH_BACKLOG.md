# 🎯 RALPH'S BACKLOG - ZeroClawed Unimplemented Features

**Created:** 2026-04-11 22:16 EDT  
**Purpose:** Guide overnight development by Ralph (or any contributor)  
**Status:** ✅ Tests intentionally fail to highlight gaps

---

## 📊 CURRENT STATUS (From Integration Test)

### ✅ **What Works:**
1. **Basic OpenAI proxy** - `/v1/chat/completions` endpoint works
2. **Model routing** - Both `deepseek-chat` and `deepseek-reasoner` work  
3. **Usage tracking** - Token counts are returned
4. **Health check** - System is responsive on VM 210

### ❌ **What's Broken/Unimplemented:**

---

## 🎯 BACKLOG ITEMS (Priority Order)

### **🔴 HIGH PRIORITY - Security & Stability**

#### 1. **Error Message Information Leakage** ❌
- **Problem:** Error messages show backend details: `"All providers failed: Backend error: HTTP request failed: API error 400 Bad Request..."`
- **Risk:** Leaks provider API endpoints, internal architecture
- **Fix:** Generic error messages (e.g., "Model not available", "Invalid request")
- **Test:** `test_error_messages_no_leakage` in `test_ralph_backlog.rs`

#### 2. **Invalid Model Handling Returns 500** ❌
- **Problem:** Requesting non-existent model returns HTTP 500 (internal error)
- **Expected:** HTTP 400 (bad request) or 404 (not found)
- **Fix:** Proper validation before routing to providers
- **Test:** `test_invalid_model_proper_error` in `test_ralph_backlog.rs`

#### 3. **Concurrent Request Deadlocks** ❌
- **Problem:** Integration test got stuck on concurrent requests
- **Risk:** System may hang under load
- **Fix:** Connection pooling, timeouts, async safety
- **Test:** `test_concurrent_requests_no_deadlock` in `test_ralph_backlog.rs`

### **🟡 MEDIUM PRIORITY - Missing Features**

#### 4. **Streaming Not Implemented** ❌
- **Problem:** `stream: true` parameter may not work
- **Expected:** Server-sent events (text/event-stream) with chunked responses
- **Fix:** Implement streaming proxy to upstream providers
- **Test:** `test_streaming_works` in `test_ralph_backlog.rs`

#### 5. **Rate Limiting Not Working** ❌
- **Problem:** No rate limiting detected in concurrent tests
- **Risk:** Resource exhaustion, cost overruns
- **Fix:** Per-API-key rate limiting, burst protection
- **Test:** `test_rate_limiting_works` in `test_ralph_backlog.rs`

#### 6. **Model Fallback Chains** ❌
- **Problem:** No fallback when primary model fails
- **Expected:** Automatic retry with backup models
- **Fix:** Configurable fallback chains in routing logic
- **Test:** `test_model_fallback_chain` in `test_ralph_backlog.rs`

### **🟢 LOW PRIORITY - Enhancements**

#### 7. **Cost Tracking Accuracy** ⚠️
- **Problem:** Token counts may not be accurate
- **Expected:** Precise prompt + completion token tracking
- **Fix:** Better token counting from provider responses
- **Test:** `test_cost_tracking_accuracy` in `test_ralph_backlog.rs`

---

## 🧪 HOW TO RUN FAILING TESTS

```bash
# 1. Check current deployment
curl -X POST http://192.168.1.210:8083/v1/chat/completions \
  -H "Authorization: Bearer test-key-123" \
  -H "Content-Type: application/json" \
  -d '{"model": "non-existent-model", "messages": [{"role": "user", "content": "test"}]}'
# ❌ Currently returns 500, should return 400

# 2. Run the backlog checker
cd /root/projects/zeroclawed
python3 check_backlog.py

# 3. Or run individual tests
./test_real_integration.sh  # Shows what works vs what's broken
```

---

## 🔧 IMPLEMENTATION GUIDANCE

### **For Error Message Fix:**
```rust
// Current (leaks info):
Err(ProviderError::AllFailed(errors)) => {
    Response::builder()
        .status(500)
        .body(format!("All providers failed: {}", errors))
}

// Fixed (generic):
Err(ProviderError::AllFailed(_)) => {
    Response::builder()
        .status(503)
        .body("Service temporarily unavailable".to_string())
}
```

### **For Invalid Model Fix:**
```rust
// Add validation before routing:
fn validate_model(model: &str) -> Result<(), ValidationError> {
    let valid_models = ["deepseek-chat", "deepseek-reasoner", "gemma-4-31b-it"];
    if !valid_models.contains(&model) {
        return Err(ValidationError::ModelNotFound(model.to_string()));
    }
    Ok(())
}
```

### **For Streaming Support:**
```rust
// Proxy streaming responses:
async fn proxy_streaming(
    upstream_response: reqwest::Response,
    mut downstream: hyper::Body
) -> Result<(), Error> {
    downstream.send_data(b"data: ").await?;
    // Forward chunks as they arrive
    while let Some(chunk) = upstream_response.chunk().await? {
        downstream.send_data(chunk).await?;
    }
    downstream.send_data(b"\n\n").await?;
    Ok(())
}
```

---

## 📈 PROGRESS TRACKING

| Item | Status | Test | Notes |
|------|--------|------|-------|
| Error leakage | ❌ Not started | `test_error_messages_no_leakage` | Security issue |
| Invalid model 500 | ❌ Not started | `test_invalid_model_proper_error` | User experience |
| Concurrent deadlocks | ❌ Not started | `test_concurrent_requests_no_deadlock` | Stability |
| Streaming | ❌ Not started | `test_streaming_works` | Feature gap |
| Rate limiting | ❌ Not started | `test_rate_limiting_works` | Resource protection |
| Fallback chains | ❌ Not started | `test_model_fallback_chain` | Resilience |
| Cost tracking | ⚠️ Partial | `test_cost_tracking_accuracy` | Enhancement |

---

## 🚀 OVERNIGHT GOALS FOR RALPH

**Minimum Viable Progress:**
1. ✅ Fix error message leakage (security)
2. ✅ Fix invalid model returning 500 (user experience)  
3. ✅ Ensure no concurrent deadlocks (stability)

**Stretch Goals:**
4. Implement basic streaming support
5. Add rate limiting configuration
6. Start model fallback chains

---

## 📝 NOTES FOR RALPH

- **Start with security issues** (error leakage)
- **Each failing test is a guide** - read the test to understand expected behavior
- **Keep tests failing** until feature is implemented (they're your TODO list)
- **Test as you go** - run `./test_real_integration.sh` after each change
- **ZeroClawed is running on VM 210** (192.168.1.210:8083)
- **API key:** `test-key-123`
- **Working models:** `deepseek-chat`, `deepseek-reasoner`

---

**Good luck, Ralph! Each failing test you fix makes ZeroClawed more production-ready.** 🚀