# ZeroClawed Test Suite Summary

**Generated:** 2026-04-12 03:47 UTC  
**Purpose:** Guide Ralph's overnight development loop

---

## 📊 Current Test Coverage

### Existing Tests (Working)

| Test File | Type | Status | Notes |
|-----------|------|--------|-------|
| `security_tests.rs` | Security | ✅ Pass | Error handling, PII leaks, injection safety |
| `property_tests.rs` | Property | ✅ Pass | URL reconstruction, adapter validation |
| `adapter_edge_cases.rs` | Integration | ✅ Pass | CLI adapter subprocess tests |
| `config_sanity.rs` | Unit | ✅ Pass | Config parsing validation |
| `loom.rs` | Concurrency | ✅ Pass | Loom-based concurrency tests |
| `mock_infrastructure.rs` | Mock | ✅ Pass | New: Message ordering, providers |
| `property_invariants.rs` | Property | ✅ Pass | New: System invariants |

### Failing Tests (RALPH BACKLOG)

| Test File | Type | Status | Priority |
|-----------|------|--------|----------|
| `ralph_backlog.rs` | Integration | ❌ FAIL | HIGH - Security & stability issues |
| `streaming_edge_cases.rs` | Unit | ✅ Pass | Ready for use |
| `adapter_specific.rs` | Unit | ✅ Pass | Ready for use |
| `performance_tests.rs` | Perf | ✅ Pass | Ready for use |

---

## 🔴 HIGH PRIORITY FAILURES (From ralph_backlog.rs)

### 1. Error Message Information Leakage
**Test:** `test_error_messages_no_leakage`  
**Problem:** Error messages show backend details: `"All providers failed: Backend error: HTTP request failed..."`  
**Fix:** Generic error messages ("Model not available", "Invalid request")  
**Location:** Proxy error handling layer

### 2. Invalid Model Returns 500
**Test:** `test_invalid_model_proper_error`  
**Problem:** Non-existent model returns HTTP 500 instead of 400/404  
**Fix:** Add validation before routing to providers  
**Location:** Model routing/validation

### 3. Concurrent Request Deadlocks
**Test:** `test_concurrent_requests_no_deadlock`  
**Problem:** Integration test got stuck on concurrent requests  
**Fix:** Connection pooling, timeouts, async safety  
**Location:** HTTP client layer

---

## 🟡 MEDIUM PRIORITY (Missing Features)

### 4. Streaming Support
**Test:** `test_streaming_works` (ralph_backlog.rs)  
**Status:** May be partially implemented  
**New Tests:** `streaming_edge_cases.rs` (12 test cases ready)

### 5. Rate Limiting
**Test:** `test_rate_limiting_works` (ralph_backlog.rs)  
**Status:** Unknown if implemented  
**New Tests:** Rate limiting simulation in `mock_infrastructure.rs`

### 6. Model Fallback Chains
**Test:** `test_model_fallback_chain` (ralph_backlog.rs)  
**Status:** Not implemented  
**Notes:** Config support needed for fallback_models array

### 7. Cost Tracking Accuracy
**Test:** `test_cost_tracking_accuracy` (ralph_backlog.rs)  
**Status:** May have bugs  
**New Tests:** Token counting validation

---

## 🆕 NEW TESTS ADDED FOR RALPH

### 1. `mock_infrastructure.rs` (12 tests)
Fast, controllable unit tests using mock infrastructure:
- Message ordering with delays
- Concurrent message handling
- Provider failure simulation
- Rate limiting simulation
- Error recovery

### 2. `property_invariants.rs` (12 property tests)
Generative testing using proptest:
- Message delivery guarantee
- Duplicate handling consistency
- Cost monotonicity
- Provider routing determinism
- Request ID uniqueness

### 3. `streaming_edge_cases.rs` (15 tests)
SSE parsing and streaming edge cases:
- Basic SSE parsing
- Multiline events
- [DONE] termination
- Malformed JSON handling
- Out-of-order chunks
- Missing chunk detection

### 4. `adapter_specific.rs` (11 tests)
Adapter validation and configuration:
- Adapter kind validation
- Timeout parsing
- Credential handling
- URL construction
- Retry configuration
- Error classification

### 5. `performance_tests.rs` (10 tests)
Performance and load testing:
- Message throughput
- Concurrent capacity
- Latency percentiles
- Burst load handling
- Connection pool limits

---

## 📋 CODEBASE TODOs FOUND

### High Priority
1. `crates/zeroclawed/src/proxy/backend.rs` - OneCLI integration TODOs (3x)
2. `crates/zeroclawed/src/proxy/streaming.rs` - SSE transformation TODO
3. `crates/zeroclawed/src/delegation.rs` - Fan-out support TODO

### Medium Priority
4. `crates/security-gateway/src/proxy.rs` - Timing tracking TODO
5. `crates/zeroclawed/src/channels/mock.rs` - Control port TODO
6. `crates/zeroclawed/src/channels/whatsapp.rs` - HMAC TODO

### Low Priority
7. `crates/zeroclawed/src/install/*` - Migration tooling TODOs
8. `crates/zeroclawed/tests/delegation_integration.rs` - Full of TODOs

---

## 🎯 RECOMMENDED OVERNIGHT WORKFLOW

### Phase 1: Security Fixes (1-2 hours)
1. Fix error message leakage in proxy layer
2. Add model validation before routing
3. Run `test_error_messages_no_leakage` - should pass
4. Run `test_invalid_model_proper_error` - should pass

### Phase 2: Stability (2-3 hours)
5. Investigate concurrent request handling
6. Add proper timeouts to HTTP client
7. Run `test_concurrent_requests_no_deadlock` - should pass

### Phase 3: Feature Implementation (3-4 hours)
8. Implement streaming support (if not done)
9. Add rate limiting configuration
10. Test with new streaming tests

### Phase 4: Test Integration (1 hour)
11. Run full test suite
12. Fix any new failures
13. Update RALPH_BACKLOG.md with progress

---

## 🔧 QUICK COMMANDS

```bash
# Run all tests
cd /root/projects/zeroclawed/crates/zeroclawed
cargo test --tests

# Run specific failing tests
cargo test test_invalid_model_proper_error -- --nocapture
cargo test test_error_messages_no_leakage -- --nocapture
cargo test test_concurrent_requests_no_deadlock -- --nocapture

# Run new tests
cargo test mock_infrastructure -- --nocapture
cargo test streaming_edge_cases -- --nocapture
cargo test performance_tests -- --nocapture

# Check deployment status
curl -X POST http://192.168.1.210:8083/v1/chat/completions \
  -H "Authorization: Bearer test-key-123" \
  -H "Content-Type: application/json" \
  -d '{"model": "deepseek-chat", "messages": [{"role": "user", "content": "test"}]}'
```

---

## ✅ SUCCESS CRITERIA

- [ ] `test_invalid_model_proper_error` passes (returns 400 not 500)
- [ ] `test_error_messages_no_leakage` passes (no internal info)
- [ ] `test_concurrent_requests_no_deadlock` passes (no hangs)
- [ ] All new unit tests pass
- [ ] Property tests pass
- [ ] Performance tests show acceptable baselines

---

**Good luck, Ralph! 🚀**