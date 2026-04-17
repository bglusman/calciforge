# ZeroClawed Test Development Guide
## Mutation Testing Oracle Gap Approach

## 🎯 Philosophy

**Test what breaks, not what works.** Use mutation testing to identify untested behavior, then create adversarial tests for those gaps.

## 🔍 Mutation Testing Workflow

1. **Baseline:** `cargo mutants` to find surviving mutants
2. **Analyze:** Identify code patterns with poor test coverage
3. **Target:** Create tests specifically for those patterns
4. **Verify:** Re-run mutation testing to confirm coverage improvement

## 📊 Identified Test Gaps (Priority Order)

### 🚨 CRITICAL (Security & Stability)

| Component | File | Test Gap | Adversarial Angle |
|-----------|------|----------|-------------------|
| **Mock Channel** | `src/channels/mock.rs` | HTTP API injection, rate limiting | Control API attacks, protocol violations |
| **Delegation System** | `src/delegation.rs` | Circular delegation, permission escalation | Privilege escalation, infinite loops |
| **Context Management** | `src/context.rs` | Memory exhaustion, serialization attacks | Context poisoning, DoS via large contexts |

### ⚠️ HIGH (Functional Correctness)

| Component | File | Test Gap | Adversarial Angle |
|-----------|------|----------|-------------------|
| **Command System** | `src/commands.rs` | Command injection, parsing edge cases | Malformed commands, privilege bypass |
| **Config Validation** | `src/config/validator.rs` | Malformed configs, injection | Config poisoning, crash induction |
| **Router** | `src/router.rs` | Message routing, race conditions | Message loss, ordering violations |

### 📝 MEDIUM (Edge Cases)

| Component | File | Test Gap | Adversarial Angle |
|-----------|------|----------|-------------------|
| **Adapters** | `src/adapters/*.rs` | Protocol translation errors | Malformed external messages |
| **Install System** | `src/install/*.rs` | File system attacks, permission issues | Path traversal, privilege escalation |
| **Proxy System** | `src/proxy/*.rs` | Authentication bypass, injection | Credential theft, man-in-the-middle |

## 🧪 Test Development Patterns

### 1. Adversarial Unit Tests
```rust
#[test]
fn test_user_id_injection() {
    // Test SQL injection, XSS, path traversal in user inputs
    let malicious_inputs = vec![
        "user'; DROP TABLE messages; --",
        "<script>alert('xss')</script>",
        "../../etc/passwd",
        "\0",  // Null byte
    ];
    
    for input in malicious_inputs {
        // Should not panic or crash
        let result = process_input(input);
        assert!(result.is_ok() || result.is_err_gracefully());
    }
}
```

### 2. Property Tests (Hegel)
```python
@given(st.lists(st.text(min_size=1, max_size=100)))
def test_no_message_loss(messages):
    """Property: All messages sent should be received."""
    # Send all messages
    # Verify each has a response
    # Should hold for any set of messages
```

### 3. Resource Exhaustion Tests
```bash
# Test memory exhaustion
dd if=/dev/urandom bs=1M count=100 | send_to_api

# Test connection exhaustion
for i in {1..1000}; do
    curl -X POST $API &
done

# Test CPU exhaustion
send_complex_processing_request &
send_complex_processing_request &
# ... repeat 100x
```

### 4. Protocol Violation Tests
```rust
// Test malformed:
// - JSON (missing fields, wrong types)
// - HTTP headers (extra large, malicious)
// - WebSocket frames (fragmented, invalid)
// - Binary protocols (wrong encoding)
```

## 🚀 Test Development Pipeline

### Phase 1: Foundation (Week 1)
1. **Mock Channel Adversarial Tests** - `test_mock_channel_adversarial.rs`
2. **Delegation Property Tests** - `test_delegation_property.rs`
3. **Basic Integration Tests** - `test_integration_adversarial.sh`

### Phase 2: Core Systems (Week 2)
1. **Command System Fuzzing** - Generate random command sequences
2. **Context Management Stress Tests** - Memory/performance under load
3. **Router Concurrency Tests** - Race conditions, message ordering

### Phase 3: External Interfaces (Week 3)
1. **Adapter Protocol Tests** - Malformed external messages
2. **Proxy Security Tests** - Authentication/authorization bypass
3. **Install System Safety Tests** - File system interactions

### Phase 4: Systemic Properties (Week 4)
1. **End-to-end Property Tests** - System invariants under all conditions
2. **Recovery Tests** - System behavior after failures
3. **Upgrade/Downgrade Tests** - Version compatibility

## 🔧 Tooling

### Mutation Testing
```bash
# Find untested code
cargo mutants

# Focus on specific modules
cargo mutants --package zeroclawed --lib

# Generate HTML report
cargo mutants --html-report
```

### Property Testing
```bash
# Run Hegel property tests
python -m pytest test_property_*.py --hypothesis-show-statistics

# Generate test cases
python -m hypothesis fuzz test_property_no_message_loss
```

### Fuzzing
```bash
# AFL++ for binary fuzzing
afl-fuzz -i testcases/ -o findings/ ./target/debug/zeroclawed

# libFuzzer for library fuzzing
cargo fuzz run parse_config
```

### Performance Testing
```bash
# Load testing
wrk -t12 -c400 -d30s http://localhost:9090/send

# Memory profiling
valgrind --leak-check=full ./target/debug/zeroclawed

# CPU profiling
perf record ./target/debug/zeroclawed
```

## 📈 Metrics & Success Criteria

### Coverage Goals
- **Line Coverage:** >90% for security-critical code
- **Branch Coverage:** >80% for all decision points
- **Mutation Score:** >80% (20% or fewer surviving mutants)

### Performance Goals
- **99th percentile latency:** <100ms under normal load
- **Memory usage:** <1GB under stress (1000 concurrent users)
- **Recovery time:** <5s after failure

### Security Goals
- **Zero crashes** from malicious inputs
- **Graceful degradation** under attack
- **No privilege escalation** via any path

## 🚨 Red Team Exercises

### Quarterly Security Review
1. **External Penetration Test** - Hire security researchers
2. **Bug Bounty Program** - Incentivize vulnerability discovery
3. **Internal Red Team** - Simulate advanced persistent threats

### Continuous Adversarial Testing
```bash
# Daily adversarial test suite
./test_adversarial_daily.sh

# Weekly fuzzing campaign
./run_fuzzing_campaign.sh

# Monthly security audit
./security_audit.sh
```

## 📚 References

### Rust Testing
- [The Rust Programming Language - Testing](https://doc.rust-lang.org/book/ch11-00-testing.html)
- [Rust by Example - Testing](https://doc.rust-lang.org/rust-by-example/testing.html)
- [cargo-mutants](https://github.com/sourcefrog/cargo-mutants)

### Property-Based Testing
- [Hypothesis (Python)](https://hypothesis.readthedocs.io/)
- [QuickCheck (Rust)](https://github.com/BurntSushi/quickcheck)
- [Proptest (Rust)](https://github.com/altsysrq/proptest)

### Security Testing
- [OWASP Testing Guide](https://owasp.org/www-project-web-security-testing-guide/)
- [MITRE ATT&CK Framework](https://attack.mitre.org/)
- [NIST SP 800-115](https://csrc.nist.gov/publications/detail/sp/800-115/final)

---

**Last Updated:** 2026-04-11  
**Next Mutation Testing Run:** Scheduled for 2026-04-18  
**Test Coverage Target:** 85% line coverage, 75% mutation score