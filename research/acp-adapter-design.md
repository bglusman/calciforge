# ACP Adapter for ZeroClawed - Design & Usage Guide

**Date:** 2026-03-18  
**Status:** Implementation Complete (using existing crates)  
**Author:** Librarian (subagent)  

> **Note:** This implementation leverages the existing ACP Rust ecosystem (`acpx`, `agent-client-protocol`, `sacp`) rather than reinventing the protocol from scratch. See `acp-adapter-implementation.md` for details on the crate architecture.

## Overview

This document describes the ACP (Agent Communication Protocol) adapter for ZeroClawed, which enables users to interact with ACP-compatible coding agents (Claude Code, Codex, OpenCode, etc.) through ZeroClawed's messaging interfaces (Telegram, Signal, WhatsApp, Matrix).

## What is ACP?

ACP (Agent Communication Protocol) is an open standard for agent communication, similar to how LSP (Language Server Protocol) standardizes IDE integrations. It uses JSON-RPC 2.0 over various transports:

- **stdio**: Spawn agent as subprocess (most common)
- **HTTP**: Connect to running agent HTTP server
- **Unix Socket**: Local socket communication

### Key ACP Resources

- **Specification:** https://github.com/i-am-bee/acp
- **Documentation:** https://agentclientprotocol.com/
- **Protocol:** JSON-RPC 2.0 with streaming support

## Architecture

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│   Telegram  │     │   Signal    │     │    Matrix   │  ┌──┤   WhatsApp  │
└──────┬──────┘     └──────┬──────┘     └──────┬──────┘  │  └─────────────┘
       │                   │                   │         │
       └───────────────────┴───────────────────┘         │
                           │                             │
                    ┌──────▼──────┐                      │
                    │  ZeroClawed   │◄─────────────────────┘
                    │   Gateway   │
                    └──────┬──────┘
                           │
              ┌────────────┼────────────┐
              │            │            │
       ┌──────▼─────┐ ┌────▼────┐ ┌────▼────┐
       │   ACP      │ │OpenClaw │ │   CLI   │
       │  Adapter   │ │ Adapter │ │ Adapter │
       └──────┬─────┘ └────┬────┘ └────┬────┘
              │            │           │
       ┌──────▼─────┐ ┌────▼────┐ ┌────▼────┐
       │Claude Code │ │ OpenClaw│ │ Custom  │
       │   Codex    │ │ Server  │ │ Scripts │
       │  OpenCode  │ │         │ │         │
       └────────────┘ └─────────┘ └─────────┘
```

## Configuration

### Basic ACP Agent Configuration

Add ACP agents to your `~/.zeroclawed/config.toml`:

```toml
version = 2

# Claude Code via stdio (most common)
[[agents]]
id = "claude-code"
kind = "acp"
transport = "stdio"
command = "claude-code"
args = ["--adapter", "acp"]
timeout_secs = 300

[agents.registry]
display_name = "Claude Code"
description = "Anthropic's Claude Code via ACP"
specialties = ["coding", "refactoring", "debugging"]
access = ["filesystem", "git"]
primary_channels = ["signal", "telegram"]

# Codex via stdio
[[agents]]
id = "codex"
kind = "acp"
transport = "stdio"
command = "codex"
args = ["serve"]
working_dir = "/home/user/projects"
env = { "OPENAI_API_KEY" = "${OPENAI_API_KEY}" }
timeout_secs = 120

[agents.registry]
display_name = "OpenAI Codex"
description = "OpenAI Codex coding agent"
specialties = ["coding", "typescript", "python"]

# ACP agent via HTTP (running as a service)
[[agents]]
id = "remote-acp"
kind = "acp"
transport = "http"
endpoint = "http://localhost:8080"
api_key = "${ACP_API_KEY}"
timeout_secs = 60

# ACP agent via Unix socket
[[agents]]
id = "socket-agent"
kind = "acp"
transport = "unix"
socket_path = "/run/acp/agent.sock"
timeout_secs = 60
```

### Routing Configuration

```toml
# Route users to default agents
[[routing]]
identity = "brian"
default_agent = "claude-code"
allowed_agents = ["claude-code", "codex"]

[[routing]]
identity = "david"
default_agent = "codex"
allowed_agents = ["codex"]
```

## Message Flow

### 1. Inbound Message Processing

```
[User Message] → [Channel Adapter] → [ZeroClawed Router] → [Identity Check]
                                                            ↓
[ACP Adapter] ← [Session Lookup] ← [Permission Check] ← [Agent Resolution]
```

### 2. ACP Protocol Translation

ZeroClawed converts its normalized message format to ACP JSON-RPC:

**ZeroClawed Message:**
```json
{
  "content": "Refactor this function to use async/await",
  "sender_id": "brian",
  "channel": "signal",
  "thread_id": "pc_abc123",
  "timestamp": "2026-03-18T10:30:00Z"
}
```

**ACP Request:**
```json
{
  "type": "request",
  "id": "req_123",
  "method": "prompt",
  "params": {
    "session_id": "acp_claude-code_uuid",
    "content": "Refactor this function to use async/await",
    "metadata": {
      "sender_id": "brian",
      "channel": "signal",
      "thread_id": "pc_abc123"
    }
  }
}
```

### 3. Session Management

ACP adapters maintain persistent sessions:

```rust
// Session created on first message
let session_id = adapter.get_or_create_session(user_id).await?;

// Session persists across messages
// Context is maintained by the ACP agent
// Last activity tracked for cleanup
```

## Steering and Confirmation Commands

### From Messaging Interface

Users can interact with ACP agents using special commands:

```
!acp status          # Check ACP agent health
!acp sessions        # List active sessions
!acp session <id>    # Show session details
!acp close <id>      # Close a session
!switch <agent>      # Switch to different agent
```

### Agent Steering

ACP supports steering commands during execution:

```json
// ACP notification from agent
{
  "type": "notification",
  "method": "steering/confirm",
  "params": {
    "prompt": "Delete file src/main.rs?",
    "action_id": "confirm_123",
    "timeout_secs": 30
  }
}
```

ZeroClawed surfaces these as messages to the user:

```
[Claude Code] Delete file src/main.rs?
Reply: !confirm confirm_123 yes
     or !confirm confirm_123 no
```

## Streaming Support

ACP agents can stream responses:

```rust
let stream = adapter.send_streaming(message).await?;

while let Some(chunk) = stream.recv().await {
    // Send chunk to user as it arrives
    channel.send(chunk.content).await?;
}
```

## Implementation Details

### File Structure

```
crates/zeroclawed/
├── src/
│   ├── lib.rs           # Main library
│   ├── main.rs          # Binary entry point
│   ├── adapters.rs      # Adapter trait definitions
│   ├── config.rs        # Configuration types
│   └── providers/
│       ├── mod.rs
│       └── acp.rs       # ACP adapter using acpx/sacp
├── Cargo.toml
└── examples/
    └── config.toml      # Example configuration
```

### External Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `acpx` | 0.1 | Thin ACP client for stdio connections |
| `agent-client-protocol` | 0.10 | Official ACP protocol types |
| `sacp` | 11 (opt) | SDK for proxies/middleware |

### Key Types

```rust
// ACP adapter using acpx
pub struct AcpAdapter {
    config: AcpAgentConfig,
    agent_id: String,
    runtime: acpx::RuntimeContext,
    agent_server: acpx::CommandAgentServer,
    sessions: Arc<RwLock<HashMap<SessionId, AcpSession>>>,
}

// Session with acpx connection
pub struct AcpSession {
    pub id: SessionId,
    pub agent_id: String,
    pub user_id: String,
    pub acp_session: Option<acpx::Connection>,
    // ...
}
```

## Testing

### Unit Tests

```rust
#[tokio::test]
async fn test_acp_stdio_connection() {
    let config = AcpAgentConfig::stdio("echo");
    let adapter = AcpAdapter::new("test".to_string(), config);
    
    assert!(adapter.health_check().await.is_ok());
}

#[tokio::test]
async fn test_message_translation() {
    let msg = Message::new("Hello", "user1", "signal");
    let acp = adapter.zeroclawed_to_acp(&msg, "session_123");
    
    assert_eq!(acp.method, "prompt");
}
```

### Integration Tests

```bash
# Test with mock ACP server
cargo test --features integration

# Test against real ACP agent
cargo test --features e2e -- --test-threads=1
```

## FAQ

### Q: Can ACP agents run as HTTP servers?

**A:** Yes, ACP supports both stdio and HTTP transports. Some agents (like custom implementations) may expose HTTP endpoints. The adapter supports:
- `stdio`: Spawn process and communicate via stdin/stdout
- `http`: POST JSON-RPC to endpoint
- `unix`: Communicate over Unix domain socket

### Q: How are long-running ACP sessions handled?

**A:** The adapter maintains session state:
1. Sessions are created per-user on first message
2. Session ID persists across messages
3. Context is maintained by the ACP agent
4. Idle sessions are cleaned up after timeout
5. Sessions can be explicitly closed via `!acp close`

### Q: How do steering commands work?

**A:** Steering flows through these steps:
1. ACP agent sends `steering/confirm` notification
2. ZeroClawed receives and surfaces to user
3. User responds with `!confirm <id> <response>`
4. ZeroClawed routes confirmation back to ACP agent
5. Agent continues with approved action

### Q: What about agent-to-agent delegation?

**A:** The `AgentRegistryInfo` metadata in config enables delegation:
```toml
[agents.claude-code.registry]
specialties = ["coding", "debugging"]
access = ["filesystem", "git"]
```

When Librarian hits an infra task, ZeroClawed can delegate to Custodian based on registry matching.

## Future Enhancements

Leveraging the existing crate ecosystem:

1. **Middleware Chains:** Use `sacp-proxy` and `sacp-tee` for composable middleware
2. **Conductor Integration:** Use `sacp-conductor` for complex proxy orchestration
3. **MCP Bridging:** Use `sacp`'s MCP server support for tool integration
4. **Session Persistence:** Leverage `sacp`'s session management
5. **Multi-Agent Routing:** Use `sacp-proxy` for intelligent request routing

See `acp-adapter-implementation.md` for detailed middleware architecture.

## References

- [ACP Specification](https://github.com/i-am-bee/acp)
- [ZeroClawed Spec](/root/.openclaw/workspace/research/zeroclawed-spec.md)
- [Adapter Trait](/root/.openclaw/workspace/crates/zeroclawed/src/adapters.rs)
- [ACP Implementation](/root/.openclaw/workspace/crates/zeroclawed/src/providers/acp.rs)
