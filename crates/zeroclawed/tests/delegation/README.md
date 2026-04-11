# Delegation Integration Tests

Docker-based integration tests for agent delegation functionality.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      Docker Network                         │
│                                                             │
│  ┌──────────┐    ┌──────────┐    ┌──────────┐              │
│  │ planner  │    │  coder   │    │ reviewer │              │
│  │  :8080   │    │  :8080   │    │  :8080   │              │
│  └────┬─────┘    └────┬─────┘    └────┬─────┘              │
│       │               │               │                     │
│       └───────────────┴───────────────┘                     │
│                       │                                     │
│              ┌────────┴────────┐                           │
│              │   zeroclawed    │                           │
│              │    :18797       │                           │
│              └─────────────────┘                           │
└─────────────────────────────────────────────────────────────┘
```

## Running Tests

### 1. Start the test environment

```bash
cd crates/zeroclawed/tests/delegation
docker-compose up --build -d
```

### 2. Wait for services to be healthy

```bash
docker-compose ps
# All services should show "healthy" or "Up"
```

### 3. Run integration tests

```bash
# From repo root
cargo test --test delegation_integration

# Or manually test via curl
curl -X POST http://localhost:18797/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "planner",
    "messages": [{"role": "user", "content": "Write a Python function to reverse a string"}]
  }'
```

### 4. Stop the test environment

```bash
docker-compose down
```

## Test Scenarios

### Scenario 1: Basic Delegation (planner → coder)

Input: "Write a Python function"

Expected flow:
1. planner receives message
2. planner delegates to coder via `::delegate::` marker
3. coder generates code
4. Response returned to user

### Scenario 2: Chained Delegation (planner → coder → reviewer)

Input: "Write and review a Python function"

Expected flow:
1. planner → coder (with "review" keyword)
2. coder writes code, delegates to reviewer
3. reviewer provides feedback
4. Response returned to user

### Scenario 3: ACL Rejection

Configure coder to only accept from planner, then try delegating from echo.

Expected: Error "target agent does not accept delegation from source"

### Scenario 4: Depth Limit

Configure chain: A → B → C → D → E → F

Expected: Error after max_depth (default 5)

### Scenario 5: Cycle Detection

Configure agents to create cycle: A → B → C → A

Expected: Error "delegation cycle detected"

## Mock Agents

The mock agents are simple Flask apps that simulate OpenAI-compatible APIs:

- **planner**: Outputs delegation markers to coder
- **coder**: Writes code, optionally delegates to reviewer
- **reviewer**: Returns code review feedback
- **echo**: Simple echo (for connectivity testing)

See `mock-agents/mock_agent.py` for implementation.

## Configuration

The test config at `fixtures/test-config.toml` defines:

1. **Agent endpoints**: Point to Docker service names
2. **Delegation ACLs**: Who can delegate to whom
3. **Max depth**: Prevent infinite chains

## Adding New Tests

1. Add new mock agent type in `mock-agents/mock_agent.py`
2. Add service to `docker-compose.yml`
3. Add agent to `fixtures/test-config.toml`
4. Write test in `tests/delegation_integration.rs`

## Debugging

View logs:
```bash
docker-compose logs -f zeroclawed
docker-compose logs -f planner
```

Shell into container:
```bash
docker-compose exec zeroclawed sh
```
