# Agent Delegation Design for ZeroClawed

## Overview
Allow configured agents to dynamically delegate to other configured agents, with:
- Opt-in ACLs per agent
- Configurable context sharing (none/recent/fork)
- Predefined workflows OR dynamic runtime delegation

## Architecture

```
┌─────────────────┐     ┌──────────────┐     ┌─────────────────┐
│  Channel Handler│────▶│   Delegation │────▶│  Router         │
│  (Telegram/etc) │     │  Middleware  │     │  (existing)     │
└─────────────────┘     └──────────────┘     └─────────────────┘
                               │
                               ▼
                        ┌──────────────┐
                        │Delegation    │
                        │Engine        │
                        │- Parse markers│
                        │- ACL checks   │
                        │- Context mgmt │
                        └──────────────┘
```

## Delegation Marker Format

Agents output delegation markers in their response:

```text
I'll delegate this coding task to the specialist.

[delegate]
target = "coder"
context = "recent"
message = "Implement auth module per the plan above"
[/delegate]
```

Or inline JSON for simpler parsing:
```text
Some analysis here...

::delegate::{"target": "coder", "context": "recent", "message": "Write tests"}::
```

## Config Schema

```toml
# Layer 1: Dynamic delegation ACLs
[[agents]]
id = "planner"
kind = "cli"
command = "/usr/local/bin/planner"
delgates = "any"  # Options: "any", "none", ["agent1", "agent2"]
accepts_from = "any"  # Who can delegate TO this agent

[[agents]]
id = "coder"
kind = "acpx"
agent_name = "codex"
delgates = ["planner", "reviewer"]  # Limited delegation
accepts_from = ["planner"]  # Only planner can call me

# Layer 2: Predefined workflows
[[workflows]]
id = "tdd-cycle"
trigger = "pattern:^/tdd"  # Regex match on message
max_depth = 3  # Prevent infinite loops

[[workflows.steps]]
agent = "red"
context = "recent"
prompt_template = "Write failing test for: {input}"

[[workflows.steps]]
agent = "green"
context = "recent"
prompt_template = "Make this pass: {previous_output}"

[[workflows.steps]]
agent = "refactor"
context = "recent"
prompt_template = "Clean up: {previous_output}"
```

## Context Modes

| Mode | Description | Use Case |
|------|-------------|----------|
| `none` | Just the delegation message | Stateless tools |
| `recent` | Last N turns (configurable, default 5) | Most conversations |
| `fork` | Copy of full context at delegation time | Isolated sub-tasks |
| `shared` | Live shared context (both see updates) | Pair programming |

## Loop Prevention

1. **Max depth counter**: Default 5 delegations per user message
2. **Cycle detection**: Track (source, target) pairs, fail on repeat
3. **Timeout**: Global delegation timeout (e.g., 5 minutes)

## Integration Points

### For ACP Adapters
ACP agents already have tool support. Delegation becomes a "system tool":

```json
{
  "type": "function",
  "function": {
    "name": "delegate_to_agent",
    "description": "Delegate task to another configured agent",
    "parameters": {
      "target": {"type": "string", "enum": ["coder", "reviewer"]},
      "context": {"type": "string", "enum": ["none", "recent"]},
      "message": {"type": "string"}
    }
  }
}
```

ZeroClawed intercepts the tool call, executes locally, returns result as tool output.

### For CLI Adapters
No native tool support. Options:
1. **Marker parsing**: Scan stdout for delegation markers
2. **Wrapper script**: CLI agent outputs markers, wrapper parses before returning
3. **Exit code signaling**: Exit 42 = delegation requested, stderr has JSON

### For OpenClaw Native
Hooks endpoint supports full tool loop. Delegation is just another tool.

## Implementation Phases

### Phase 1: Minimal viable
- Static delegation markers in responses
- One-hop only (no chaining)
- Recent context only
- CLI adapter marker parsing

### Phase 2: Chaining + Config
- Config file support for ACLs
- Multi-hop with depth limit
- Workflow definitions

### Phase 3: Native Tools
- ACP/OpenClaw native tool integration
- Real-time delegation status to user

## Docker Integration Testing

See `tests/delegation/` directory structure:

```
tests/delegation/
├── docker-compose.yml          # Multi-agent test environment
├── test-agents/
│   ├── planner/               # Mock agent that delegates
│   │   ├── Dockerfile
│   │   └── delegate.sh        # Outputs delegation marker
│   ├── coder/                 # Mock agent that "codes"
│   │   ├── Dockerfile
│   │   └── code.sh
│   └── reviewer/              # Mock agent that "reviews"
├── fixtures/
│   └── delegation-config.toml
└── integration_tests.rs
```

### Test Scenarios

1. **Basic delegation**: planner → coder
2. **Chained delegation**: planner → coder → reviewer  
3. **ACL rejection**: unauthorized agent tries to delegate
4. **Loop prevention**: A → B → A (should fail)
5. **Context modes**: verify none/recent/fork behavior
6. **Timeout**: slow delegate should error

### Mock Agent Implementation

Simple HTTP agents for testing:

```python
# mock_agent.py
from flask import Flask, request, jsonify
import os

app = Flask(__name__)
AGENT_TYPE = os.environ['AGENT_TYPE']  # planner|coder|reviewer

@app.route('/v1/chat/completions', methods=['POST'])
def chat():
    message = request.json['messages'][-1]['content']
    
    if AGENT_TYPE == 'planner':
        return jsonify({
            'choices': [{
                'message': {
                    'content': f'Plan: {message}\n\n::delegate::{{"target": "coder", "context": "recent", "message": "Implement the plan"}}::'
                }
            }]
        })
    elif AGENT_TYPE == 'coder':
        return jsonify({
            'choices': [{
                'message': {
                    'content': f'Code for: {message}'
                }
            }]
        })
    # ...

if __name__ == '__main__':
    app.run(host='0.0.0.0', port=8080)
```

## Research: Existing Agent Extension Patterns

### OpenAI Assistants API
- Tools defined at assistant creation time
- Function calling: assistant → user (who executes) → assistant
- Our twist: ZeroClawed executes, not user

### ACP (Agent Communication Protocol)
- Native tool support via `tools/call` endpoint
- Tool definitions in `InitializeResponse`
- Our integration: delegation as built-in tool

### MCP (Model Context Protocol)
- External tool servers
- stdio or HTTP transport
- Our delegation is internal, not external

### LangChain/LangGraph
- Chains: predefined sequences
- Graphs: conditional routing
- Our approach: runtime delegation decided by agent

## Open Questions

1. **UX**: Show delegation chain to user? ("planner → coder → reviewer")
2. **Billing**: Track costs per sub-call?
3. **Interruption**: Can user cancel mid-delegation-chain?
4. **State**: Persist partial chains across restarts?
5. **Parallel**: Allow fan-out (one agent → multiple delegates)?

## Next Steps

1. Implement marker parsing in CLI adapter
2. Add delegation engine skeleton
3. Docker test harness
4. Config schema validation

## Appendix: Slash Command Interception (Related Feature)

For agents where native tool/slash command support isn't working (e.g., some OpenClaw native configurations, CLI adapters), ZeroClawed can intercept and execute locally.

### Use Case

**Current broken flow (OpenClaw native with hooks):**
```
User: /status
ZeroClawed → Agent: "/status"
Agent → ZeroClawed: "<tool_call><tool_name>session_status</tool_name>...</tool_call>"
ZeroClawed → User: [raw XML shown, not executed]
```

**Working flow (ZeptoClaw/Lucien):**
```
User: /status
ZeroClawed → Agent: "/status"
Agent → ZeptoClaw: tool call
ZeptoClaw → executes → returns result
Agent → User: formatted status
```

**With ZeroClawed interception (fallback for broken native):**
```
User: /status
ZeroClawed: [intercepts /status command]
ZeroClawed: executes locally or delegates to agent that can execute
ZeroClawed → Agent: "[Tool result: status info]\n\nOriginal: /status"
Agent → User: natural language response using injected result
```

### Config

```toml
[[agents]]
id = "codex"
kind = "acpx"
agent_name = "codex"
# Enable slash command interception for this agent
intercept_slash_commands = true
# Which commands to intercept (or "all")
allowed_slash_commands = ["search", "read", "edit", "status"]
```

### Shared Infrastructure with Delegation

| Feature | Delegation | Slash Commands |
|---------|------------|----------------|
| Marker parsing | `::delegate::` | `/command` prefix |
| Execution | Route to other agent | Execute tool locally |
| ACLs | `delegates`, `accepts_from` | `allowed_slash_commands` |
| Response injection | Return to source agent | Return to source agent |

Both features use the same interception pattern: **parse special markers → execute → inject result**.
