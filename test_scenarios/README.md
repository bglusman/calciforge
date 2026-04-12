# ZeroClawed Test Scenarios

## Directory Structure
```
test_scenarios/
├── README.md                    # This file
├── basic/                       # Basic functionality tests
│   ├── channel_integration/     # Channel ↔ Agent integration
│   ├── proxy_basics/           # Proxy server functionality
│   └── alloy_selection/        # Alloy model selection
├── adversarial/                 # Adversarial/failure tests
│   ├── injection/              # Injection attacks
│   ├── resource_exhaustion/    # DoS/resource attacks
│   ├── byzantine/              # Byzantine failures
│   └── timing/                 # Timing attacks
├── property/                   # Property-based tests
│   ├── invariants/             # System invariants
│   ├── fuzzing/                # Fuzz tests
│   └── generative/             # Generative tests
├── mutation/                   # Mutation tests
│   ├── message_mutation/       # Message mutation
│   ├── response_mutation/      # Response mutation
│   └── behavior_mutation/      # Behavior mutation
└── integration/                # Integration scenarios
    ├── e2e_flows/             # End-to-end flows
    ├── error_recovery/        # Error recovery flows
    └── configuration/         # Configuration scenarios
```

## Scenario Format

Each scenario is a TOML file with the following structure:

```toml
# test_scenarios/basic/channel_integration/simple_echo.toml
name = "Simple Echo Test"
description = "Basic message echo through channel → agent → response"
category = "basic.channel_integration"
priority = "high"

[setup]
config = "test_simple_direct.toml"
channels = ["mock"]
agents = ["test-agent"]

[scenario]
steps = [
    { action = "send_message", channel = "mock", sender = "user1", text = "Hello" },
    { action = "wait_response", timeout = "5s" },
    { action = "verify_response", contains = "Hello" }
]

[assertions]
all_of = [
    "response_received_within_5s",
    "response_contains_original_message",
    "no_errors_logged"
]

[expected_outcome]
result = "success"
max_duration = "10s"
```

## Running Tests

```bash
# Run all tests in a category
cargo test --test adversarial

# Run specific scenario
cargo test --test scenario -- simple_echo

# Run with different configurations
ZEROCLAWED_TEST_CONFIG=test_mock_channel.toml cargo test
```

## Test Utilities

### Mock Channel Control
```rust
// Send test message
mock_channel.send("user1", "Hello world");

// Verify responses
let responses = mock_channel.get_responses();
assert!(responses.len() > 0);
```

### LLM Response Mocking
```rust
// Mock LLM response with tool call
let mock_response = MockLlmResponse::new()
    .with_content("I'll help with that")
    .with_tool_call("web_search", json!({"query": "test"}));

// Mock tool execution result
let tool_result = MockToolResult::new()
    .with_success(json!({"results": []}));
```

### Failure Injection
```rust
// Inject network failure
failure_injector.inject_network_failure(Duration::from_secs(2));

// Inject LLM provider error
failure_injector.inject_provider_error("rate_limit_exceeded");

// Inject channel corruption
failure_injector.inject_message_corruption(0.1); // 10% corruption rate
```

## Adding New Scenarios

1. Create TOML file in appropriate directory
2. Implement test runner if needed
3. Add to test suite in `tests/integration.rs`
4. Document scenario purpose and expected behavior

## Continuous Testing

Scenarios are run:
- On every PR: Basic + High priority adversarial
- Nightly: All tests except chaos
- Weekly: Full suite including chaos tests
