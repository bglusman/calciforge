# ACP Adapter for PolyClaw - Implementation Guide

**Date:** 2026-03-18  
**Status:** Refactored to use existing ACP crates  
**Author:** Librarian (subagent)  

## Overview

This implementation of the ACP adapter for PolyClaw leverages the existing Rust crate ecosystem instead of reinventing the wheel:

| Crate | Purpose | Used For |
|-------|---------|----------|
| `acpx` | Thin ACP client | Stdio connections to agents |
| `agent-client-protocol` | Official ACP types | Protocol message types |
| `sacp` | Symposium's ACP SDK | Middleware/proxy chains |
| `sacp-proxy` | Message routing | Request/response routing |
| `sacp-tee` | Stream duplication | Logging and debugging |
| `sacp-conductor` | Proxy orchestration | Chain management |

## Architecture

### Without Middleware (Basic)

```
PolyClaw -> acpx -> ACP Agent (stdio)
```

### With Middleware (Advanced)

```
PolyClaw -> sacp-proxy -> sacp-tee -> ACP Agent
                ↓           ↓
           [routing]   [logging]
```

## Key Crates Explained

### 1. `acpx` - The Foundation

From [docs.rs/acpx](https://docs.rs/acpx):

> *"acpx is a thin Rust client for launching ACP-compatible agent servers and talking to them through the official Agent Client Protocol (ACP) Rust SDK. The crate stays close to upstream ACP types and lifecycle rules."*

**Key types:**
- `Connection` - ACP connection handle
- `RuntimeContext` - Async runtime integration
- `CommandAgentServer` - Spawn agent subprocesses

### 2. `sacp` - The SDK

From [docs.rs/sacp](https://docs.rs/sacp):

> *"sacp is a Rust SDK for building Agent-Client Protocol (ACP) applications. ACP is a protocol for communication between AI agents and their clients (IDEs, CLIs, etc.), enabling features like tool use, permission requests, and streaming responses."*

**Key abstractions:**
- `Client` - Connect to agents
- `Proxy` - Intercept/transform messages
- `Builder` - Compose connections

### 3. `sacp-proxy` - Message Routing

From [deepwiki.com](https://deepwiki.com/agentclientprotocol/rust-sdk/3.4-middleware-components):

> *"The sacp-proxy crate provides high-performance message routing capabilities using the fxhash hashing algorithm. It enables multiplexing multiple agent-client connections through a single routing point."*

**Use cases:**
- Route messages between multiple agents
- Load balancing
- Request/response correlation

### 4. `sacp-tee` - Stream Duplication

> *"Stream duplication for debugging and logging - observe protocol traffic without affecting the main flow."*

**Use cases:**
- Audit logging
- Debug tracing
- Traffic analysis

### 5. `sacp-conductor` - Orchestration

> *"Conductor for orchestrating SACP proxy chains. Spawns and manages proxy components, routes messages between them."*

**Use cases:**
- Complex middleware chains
- Dynamic proxy composition
- Lifecycle management

## Implementation Details

### Basic Adapter (using `acpx`)

```rust
use acpx::{AgentServerMetadata, CommandAgentServer, CommandSpec, RuntimeContext};
use agent_client_protocol as acp;

pub struct AcpAdapter {
    runtime: RuntimeContext,
    agent_server: CommandAgentServer,
    sessions: Arc<RwLock<HashMap<SessionId, AcpSession>>>,
}

impl AcpAdapter {
    pub fn new(config: AcpAgentConfig) -> Result<Self> {
        let runtime = RuntimeContext::new(|task| {
            tokio::runtime::Handle::current().block_on(task);
        });

        let metadata = AgentServerMetadata::new(
            "claude-code",
            "Claude Code",
            "0.1.0",
        );

        let cmd_spec = CommandSpec::new("claude-code")
            .arg("--adapter")
            .arg("acp");

        let agent_server = CommandAgentServer::new(metadata, cmd_spec);

        Ok(Self { runtime, agent_server, ... })
    }

    async fn connect(&self) -> Result<acpx::Connection> {
        self.agent_server.connect(&self.runtime).await
    }
}
```

### With Middleware (using `sacp-proxy`)

```rust
#[cfg(feature = "acp-middleware")]
pub struct MiddlewareAcpAdapter {
    base: AcpAdapter,
    proxy: sacp_proxy::Proxy,
}

impl MiddlewareAcpAdapter {
    pub async fn send_with_middleware(&self, msg: Message) -> Result<Message> {
        // Route through proxy chain
        self.proxy.route(msg).await
    }
}
```

### Middleware Chain Example

```rust
use sacp_conductor::Conductor;
use sacp_proxy::Proxy;
use sacp_tee::Tee;

// Build middleware chain
let conductor = Conductor::new()
    .add_proxy(Tee::new().log_to_file("acp-traffic.log"))
    .add_proxy(Proxy::new().route_to("claude-code"))
    .add_proxy(AuthMiddleware::new(token));

// Run the chain
conductor.run().await?;
```

## Configuration

### TOML Configuration

```toml
[[agents]]
id = "claude-code"
kind = "acp"
command = "claude-code"
args = ["--adapter", "acp"]

# Enable middleware (optional)
enable_middleware = true

[[agents.middleware]]
kind = "tee"
log_file = "/var/log/polyclaw/acp-tee.log"

[[agents.middleware]]
kind = "auth"
token_env = "ACP_AUTH_TOKEN"

[[agents.middleware]]
kind = "proxy"
routing_table = "round-robin"
```

## Middleware Components

### Auth Middleware

```rust
pub struct AuthMiddleware {
    token: String,
}

impl AcpMiddleware for AuthMiddleware {
    async fn transform_request(&self, req: acp::PromptRequest) -> acp::PromptRequest {
        req.with_context(acp::Context::new()
            .with_meta("auth_token", self.token.clone()))
    }
}
```

### Logging Middleware (using `sacp-tee`)

```rust
use sacp_tee::Tee;

let tee = Tee::new()
    .with_output(std::io::stdout())
    .with_format(TeeFormat::Json);
```

### Routing Middleware (using `sacp-proxy`)

```rust
use sacp_proxy::Proxy;

let proxy = Proxy::new()
    .with_backend("claude-code", claude_connection)
    .with_backend("codex", codex_connection)
    .with_router(|req| {
        if req.content.contains("rust") {
            "claude-code"
        } else {
            "codex"
        }
    });
```

## Comparison: Custom vs. Existing Crates

| Aspect | Custom Implementation | Using Existing Crates |
|--------|----------------------|----------------------|
| Lines of Code | ~800 | ~200 |
| Maintenance | Full burden | Community maintained |
| Protocol Updates | Manual | Automatic via crates |
| Middleware | Custom implementation | `sacp-proxy`, `sacp-tee` |
| Testing | Write all tests | Leverage crate tests |
| Documentation | Write all docs | Existing docs.rs |

## Migration Path

### From Custom to `acpx`

**Before:**
```rust
// Custom stdio handling
let child = Command::new("claude-code").spawn()?;
let stdin = child.stdin.take()?;
// Manual JSON-RPC framing...
```

**After:**
```rust
// Using acpx
let server = CommandAgentServer::new(metadata, cmd_spec);
let connection = server.connect(&runtime).await?;
let response = connection.prompt(request).await?;
```

### Adding Middleware

**Step 1:** Enable feature
```toml
[features]
acp-middleware = ["dep:sacp", "dep:sacp-proxy", "dep:sacp-tee"]
```

**Step 2:** Wrap adapter
```rust
let base = AcpAdapter::new(config)?;
let with_middleware = MiddlewareAcpAdapter::new(base)
    .with_tee(Tee::new())
    .with_proxy(Proxy::new());
```

## Best Practices

1. **Use `acpx` for basic connectivity** - Don't implement stdio handling yourself
2. **Use `sacp` for proxies** - Leverage the component abstraction
3. **Use `sacp-conductor` for complex chains** - Don't manage proxies manually
4. **Feature-gate middleware** - Keep basic adapter lightweight
5. **Follow ACP lifecycle** - Initialize, run, close properly

## References

- [acpx on crates.io](https://crates.io/crates/acpx)
- [sacp on crates.io](https://crates.io/crates/sacp)
- [sacp-conductor on crates.io](https://crates.io/crates/sacp-conductor)
- [ACP Specification](https://github.com/i-am-bee/acp)
- [Middleware Components](https://deepwiki.com/agentclientprotocol/rust-sdk/3.4-middleware-components)
- [Proxy Chains RFD](https://agentclientprotocol.com/rfds/proxy-chains)
