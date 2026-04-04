# ACP Session Discovery, Multi-Client Attachment, and Handoff Research

**Date:** 2026-03-17  
**Researcher:** Librarian Subagent  
**Topic:** ACP (Agent Communication Protocol) session capabilities for desktop→mobile→desktop handoff use case

---

## Executive Summary

**Verdict: Partially feasible with significant workarounds.**

ACP (Agent Communication Protocol) provides session persistence and distributed session capabilities, but **does not natively support multi-client attachment or real-time session handoff** between clients. The use case of seamlessly handing off a live coding session from desktop (Claude Code/OpenCode) to mobile (ZeroClawed) and back is **not directly supported** by the protocol as currently implemented.

However, there are viable workaround architectures that could achieve similar functionality.

---

## 1. Session Discovery

### What ACP Provides

| Feature | Status | Details |
|---------|--------|---------|
| **Agent Discovery** | ✅ Implemented | Agents advertise capabilities via Agent Manifests embedded in distribution packages |
| **Agent Registry** | ✅ Implemented | Tree-based or federated registry structures support lookup |
| **Session Enumeration** | ❌ Not implemented | No protocol-level discovery of "running sessions" |
| **Local Process Discovery** | ❌ Not implemented | No mDNS/zeroconf-style discovery for local ACP processes |

### Key Findings

- **Agent Discovery exists, Session Discovery does not**: ACP allows agents to advertise their capabilities via `Agent Manifests`, but there's no mechanism to discover *active running sessions* on a local machine.
- **URI-based addressing**: Sessions are addressed via `acp://` URIs (e.g., `acp://agents/echo`), but these identify agents, not active session instances.
- **Offline discovery supported**: Agents can be discovered even when inactive via embedded manifests, supporting scale-to-zero environments.

### Relevant Documentation
> "ACP implements a pluggable storage architecture that supports multiple backends for session persistence, message history, and distributed deployments." — DeepWiki ACP System Architecture

---

## 2. Multi-Client Attachment

### What ACP Provides

| Feature | Status | Details |
|---------|--------|---------|
| **Multiple simultaneous clients** | ❌ Not supported | ACP sessions are 1:1 client-server |
| **Observer mode** | ❌ Not implemented | No read-only session observation |
| **Controller handoff** | ❌ Not implemented | No formal session ownership transfer |
| **Session resumption** | ⚠️ Partial | Sessions can be resumed, not simultaneously accessed |

### Key Findings

- **Session/Run model**: ACP uses a "run" abstraction where each interaction creates a run within a session. Sessions persist state, but active runs are single-client.
- **Client-server architecture**: From the DeepLearning.AI course: *"The protocol is based on a client-server architecture: you host an agent built with any framework inside an ACP server, and send requests to the server through an ACP client."*
- **No concurrent access**: Multiple clients cannot simultaneously connect to the same active session. A new client creating a session creates a NEW session.

### Evidence from OpenCode Issue
A GitHub issue on the OpenCode repo (anomalyco/opencode#8931) requested explicit session management commands:
```javascript
// List available sessions
acp.session.list({ limit?: number, projectPath?: string }) → Session[]
// Switch to existing session  
acp.session.switch({ sessionId: string }) → void
```

This confirms that **session switching is not natively supported** and requires explicit implementation.

---

## 3. Session Handoff / Resumption

### What ACP Provides

| Feature | Status | Details |
|---------|--------|---------|
| **Session persistence** | ✅ Implemented | Sessions maintain state and conversation history |
| **Distributed sessions** | ✅ Implemented | Session continuity across server instances via URI-based resource sharing |
| **Session serialization** | ✅ Implemented | Session content stored via HTTP URLs on external resource servers (S3-compatible) |
| **Real-time handoff** | ❌ Not implemented | No "pause desktop, resume mobile" capability |
| **Session ownership transfer** | ❌ Not implemented | No formal ownership model |

### Key Findings

#### Distributed Sessions (The Key Capability)
From the ACP documentation:
> "In distributed sessions, **session content is referenced by HTTP URL and stored on an arbitrary resource server** (e.g. S3-compatible server). When a session moves between ACP servers, only the session descriptor needs to be forwarded."

This is the most relevant feature for the handoff use case. It means:
1. Session state can be externalized to a shared storage backend
2. Different clients can theoretically access the same session data
3. However, **synchronization is not handled automatically**

#### Session Resume vs. Session Load
From the Agent Client Protocol specification (agentclientprotocol.com):
- `session/load`: For loading conversations started by the client itself
- `session/resume`: Effectively a subset of session/load, decoupled from storage mechanics

From a Gemini CLI issue:
> "There is no way to skip session/new step to reuse pre session_id to load session... with acp mode, gemini cli generate more and more session and acp client can not use previous session."

This indicates **resuming existing sessions is not straightforward** in current implementations.

---

## 4. Implementation Approaches

### Option A: Session Proxy Architecture (Recommended)

**Concept:** A lightweight "session proxy" that both desktop and mobile clients connect to, maintaining the canonical session state.

```
┌─────────────┐      ┌──────────────┐      ┌─────────────┐
│   Desktop   │◄────►│ Session      │◄────►│  ACP Agent  │
│ (Claude Code│      │ Proxy        │      │ (OpenCode)  │
│ /OpenCode)  │      │ (ZeroClawed?)  │      │             │
└─────────────┘      └──────┬───────┘      └─────────────┘
                            │
                     ┌──────┴──────┐
                     │    Mobile   │
                     │  (ZeroClawed) │
                     └─────────────┘
```

**How it works:**
1. Desktop client connects to Session Proxy via ACP
2. Session Proxy maintains the actual session with the ACP Agent
3. When user leaves desktop, proxy continues maintaining session state
4. Mobile connects to same Session Proxy, picks up where desktop left off
5. Desktop reconnects later, syncs to current state

**Pros:**
- No protocol changes needed
- Proxy can handle the "orchestration" between clients
- Could support true real-time handoff with proper proxy design

**Cons:**
- Requires building and running the proxy infrastructure
- Single point of failure
- Adds latency

### Option B: Externalized Session State

**Concept:** Use ACP's distributed sessions feature with a shared storage backend that both clients can access.

**How it works:**
1. Configure ACP agents to use external session storage (S3-compatible)
2. Desktop client creates session, session ID is stored
3. Mobile client "resumes" session by loading the same session ID from shared storage
4. Both clients explicitly sync via the shared storage

**Pros:**
- Uses built-in ACP distributed session capabilities
- No proxy infrastructure needed

**Cons:**
- Not real-time (clients must explicitly save/load)
- Potential conflict if both try to write simultaneously
- Requires session ID to be passed between devices (QR code, deep link, etc.)

### Option C: Agent-Level Handoff Support

**Concept:** Modify or extend the ACP agent (Claude Code/OpenCode) to support multiple client connections.

**How it works:**
1. Extend the agent to maintain a client registry
2. Implement observer vs controller modes in the agent
3. Allow controller handoff via explicit messaging

**Pros:**
- Most seamless user experience
- True real-time handoff possible

**Cons:**
- Requires modifying existing agents (Claude Code is closed-source)
- Non-standard ACP behavior
- Complex to implement correctly

### Option D: Session Snapshot/Restore

**Concept:** Simple "export on desktop, import on mobile" workflow.

**How it works:**
1. Desktop client exports session state to a file/shared URL
2. User transfers to mobile (AirDrop, cloud storage, QR code)
3. Mobile client imports session state
4. Mobile takes over as the active client

**Evidence:** OpenCode already supports this:
> "Import session data from a JSON file or OpenCode share URL... You can import from a local file or an OpenCode share URL."

**Pros:**
- Simple to implement
- Works with existing tools
- No infrastructure needed

**Cons:**
- Not real-time (explicit export/import required)
- No simultaneous access
- User-initiated only

---

## 5. Feasibility Assessment

### Use Case: Desktop → Mobile → Desktop Handoff

| Approach | Real-time? | User Effort | Complexity | Feasibility |
|----------|-----------|-------------|------------|-------------|
| Session Proxy | Yes | Low | High | ⚠️ Moderate |
| Externalized State | No | Medium | Medium | ✅ High |
| Agent Modification | Yes | Low | Very High | ❌ Low |
| Snapshot/Restore | No | High | Low | ✅ High |

### Recommended Architecture: Hybrid Approach

For the ZeroClawed use case, a **Session Proxy with Snapshot Fallback** architecture is recommended:

1. **Primary flow**: ZeroClawed runs a lightweight session proxy that both desktop and mobile connect to
2. **Fallback flow**: When proxy unavailable, use explicit session export/import via share URLs
3. **Sync mechanism**: Use ACP's distributed session storage with external S3-compatible backend

---

## 6. Open Questions / Further Research Needed

1. **Does ZeroClawed already implement any session proxy functionality?** The Big Hat Group blog mentions using `sessions_spawn` with `runtime: "acp"` for persistent sessions - investigate if this can be leveraged.

2. **What is the exact session storage format?** Need to examine the ACP Python SDK's `context.session.load_history()` and `context.session.store_state()` implementations.

3. **How does Claude Code's ACP implementation handle sessions?** The `acp-claude-code` wrapper mentions "automatic conversation persistence and session resumption capabilities" - examine this implementation.

4. **Is there a WebSocket transport for ACP?** Some sources mention WebSocket support for browser-based agents, which would enable real-time proxy architectures.

---

---

## Appendix A: Client-Specific Session Handoff Mechanisms

**Research Date:** 2026-03-17 (Addendum)

This section documents practical, client-specific mechanisms for session handoff that can be leveraged by ZeroClawed, regardless of ACP protocol limitations.

---

### A.1 Claude Code: Remote Control Feature

Claude Code has a **built-in session handoff mechanism** called "Remote Control" that is **not ACP-based** but proprietary to Anthropic's infrastructure.

#### How It Works

| Aspect | Details |
|--------|---------|
| **Command** | `claude remote-control` (or `/rc` shorthand) |
| **Mechanism** | Local session registers with Anthropic API, polls for work |
| **Connectivity** | Outbound HTTPS only — no inbound ports required |
| **Mobile Access** | Session URL + QR code displayed in terminal |
| **Network** | Works behind NAT, firewalls, home routers (Tailscale/ngrok pattern) |

#### Session Handoff Flow

```
┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
│  Desktop Terminal│────►│  Anthropic API   │◄────│  Mobile Browser │
│  (claude rc)     │     │  (relay service) │     │  (claude.ai)    │
└─────────────────┘     └──────────────────┘     └─────────────────┘
        │                                                   │
        │  1. Desktop registers session                     │
        │  2. Desktop polls for work                        │
        │                                          3. Mobile connects via URL
        │  4. API relays messages bidirectionally          │
```

#### Key Findings for ZeroClawed Integration

| Feature | Status | Notes |
|---------|--------|-------|
| **Session resumption** | ❌ Not supported | `--resume` doesn't work with remote-control |
| **One-way handoff** | ✅ Supported | Terminal → Web only (web cannot hand back to terminal) |
| **QR code display** | ✅ Built-in | Spacebar toggles QR code in terminal |
| **Concurrent access** | ❌ No | Only one controlling client at a time |
| **Subscription required** | ⚠️ Max tier only | $100-200/month (Pro access rumored) |

#### ZeroClawed Integration Path

**Cannot directly integrate** - Claude Code Remote Control is a closed, proprietary system that requires:
- Claude Max subscription
- Claude mobile app or claude.ai/code access
- No API for third-party clients

**Workaround:** Use Claude Code's `--remote` flag to create web sessions, then access via ZeroClawed's browser capabilities if it has web view support.

---

### A.2 OpenCode: Native HTTP Server + Multi-Client Support

OpenCode has **excellent support for session sharing** through its HTTP server mode, which is **directly exploitable** by ZeroClawed.

#### Server Mode Capabilities

| Feature | Command | Details |
|---------|---------|---------|
| **Headless server** | `opencode serve` | HTTP API without TUI |
| **Web UI server** | `opencode web` | Serves web interface + API |
| **Default port** | 4096 | Configurable via `--port` |
| **Default hostname** | 127.0.0.1 | Configurable via `--hostname` |
| **Authentication** | `OPENCODE_SERVER_PASSWORD` | HTTP basic auth |
| **mDNS discovery** | `--mdns` | Auto-discovery on LAN |

#### Multi-Client Architecture

OpenCode's server mode **explicitly supports multiple simultaneous clients**:

```
┌─────────────────────────────────────────────────────────────────┐
│                    OpenCode Server (port 4096)                  │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐             │
│  │   TUI #1    │  │   TUI #2    │  │   Web UI    │  ← Multiple │
│  │  (attach)   │  │  (attach)   │  │  (browser)  │    clients  │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘             │
│         └─────────────────┼─────────────────┘                   │
│                           ▼                                     │
│                    Shared Session State                         │
│              (SQLite backend, v1.2.0+)                          │
└─────────────────────────────────────────────────────────────────┘
```

From the docs:
> "You can attach a terminal TUI to a running web server... This allows you to use both the web interface and terminal simultaneously, sharing the same sessions and state."

#### Key API Endpoints

Based on documentation and GitHub issues, the OpenCode HTTP API includes:

| Endpoint | Purpose |
|----------|---------|
| `GET /` | OpenAPI 3.1 spec |
| `POST /session` | Create new session |
| `GET /session/{id}` | Get session info |
| `POST /session/{id}/message` | Send message to session |
| `GET /session/{id}/messages` | Get message history |
| `POST /tui` | Drive TUI programmatically |
| `GET /events` | SSE stream for real-time events |

#### Session Handoff Flow

**Desktop → Mobile handoff:**

```bash
# Terminal 1: Start headless server on desktop
$ opencode serve --port 4096 --hostname 0.0.0.0

# Terminal 2: Attach TUI to server (optional)
$ opencode attach http://localhost:4096

# Mobile: Connect to server via ZeroClawed tunnel
# Access http://desktop-ip:4096 or tunneled URL
```

**Critical:** OpenCode v1.2.0+ migrated session storage to SQLite, enabling persistence across reconnections.

#### ZeroClawed Integration Path

| Approach | Feasibility | Implementation |
|----------|-------------|----------------|
| **Direct HTTP proxy** | ✅ High | Proxy OpenCode's HTTP API through ZeroClawed |
| **Tailscale integration** | ✅ High | Use Tailscale for secure mobile→desktop routing |
| **SSE event streaming** | ✅ High | ZeroClawed can consume real-time event stream |
| **Session import/export** | ✅ Built-in | `opencode export` / `opencode import` commands |

**Recommended Architecture:**

```
┌──────────────┐     ┌──────────────┐     ┌──────────────────┐
│   Mobile     │◄───►│   ZeroClawed   │◄───►│  OpenCode Server │
│  (ZeroClawed)  │     │   (proxy)    │     │  (desktop)       │
└──────────────┘     └──────────────┘     └──────────────────┘
                           │                         │
                           │    ┌────────────────────┘
                           │    │
                           ▼    ▼
                    ┌──────────────────┐
                    │  Tailscale/VPN   │  ← Optional: secure routing
                    └──────────────────┘
```

---

### A.3 Claude Code Web Sessions (claude --remote)

Separate from Remote Control, Claude Code can spawn **web sessions** that run on Anthropic's cloud infrastructure.

#### How It Works

| Aspect | Details |
|--------|---------|
| **Command** | `claude --remote` |
| **Mechanism** | Terminal session uploaded to claude.ai/code |
| **Handoff** | One-way: Terminal → Web only |
| **Context** | Current conversation history transferred |

From the docs:
> "The `&` prefix creates a new web session with your current conversation context."

#### Limitations for ZeroClawed

| Limitation | Impact |
|------------|--------|
| One-way only | Cannot return from web to terminal |
| Cloud execution | Code runs on Anthropic's servers, not local |
| No local session persistence | Original terminal session ends |
| Requires subscription | Max tier |

---

### A.4 Practical Integration Matrix

| Client | Mechanism | Multi-Client | Bidirectional Handoff | ZeroClawed Integration |
|--------|-----------|--------------|----------------------|---------------------|
| **Claude Code** | Remote Control | ❌ No | ❌ One-way | ❌ Not possible (closed) |
| **Claude Code** | `--remote` flag | N/A | ❌ One-way | ⚠️ Browser-only |
| **OpenCode** | `serve` + `attach` | ✅ Yes | ✅ Yes | ✅ **Recommended** |
| **OpenCode** | `web` command | ✅ Yes | ✅ Yes | ✅ **Recommended** |

---

### A.5 Recommended ZeroClawed Architecture

Based on this research, the optimal approach for ZeroClawed:

#### Phase 1: OpenCode Integration (Immediate)

```yaml
# ZeroClawed configuration snippet
opencode:
  enabled: true
  # Auto-detect local OpenCode servers via mDNS
  discovery: mdns
  # Or specify explicit servers
  servers:
    - name: "Desktop"
      url: "http://10.0.0.90:4096"
      auth:
        username: opencode
        password: ${OPENCODE_SERVER_PASSWORD}
  
  # Session handoff settings
  handoff:
    # Keep sessions alive when switching clients
    persistent: true
    # Sync interval for mobile handoff
    sync_interval: 5s
    
  # Tailscale integration for secure mobile access
  tailscale:
    enabled: true
    funnel: true  # Expose via Tailscale Funnel for mobile
```

#### Phase 2: Generic ACP Proxy (Future)

For clients that don't have native server modes:

```yaml
acp_proxy:
  enabled: true
  # Run local ACP proxy that maintains session state
  proxy_command: "acp-proxy --port 8080"
  
  # Clients connect to proxy instead of directly to agent
  clients:
    - name: "Claude Code"
      command: "claude"
      proxy_to: "http://localhost:8080/claude"
    - name: "OpenCode"
      command: "opencode"
      proxy_to: "http://localhost:8080/opencode"
```

---

## Appendix B: Summary of Integration Options

### Option 1: OpenCode Native (Recommended)

**Best for:** Users who can adopt OpenCode as their primary agent

| Pros | Cons |
|------|------|
| Native multi-client support | Requires switching from Claude Code |
| Built-in HTTP API | OpenCode may lack some Claude Code features |
| Session persistence in SQLite | |
| `attach` command for reconnection | |

**Implementation:**
1. User runs `opencode serve` on desktop
2. ZeroClawed discovers server via mDNS or explicit config
3. ZeroClawed proxies HTTP API to mobile interface
4. User switches between desktop TUI and mobile seamlessly

### Option 2: Session Snapshot/Restore (Universal)

**Best for:** Supporting any ACP-compatible agent

**Implementation:**
1. ZeroClawed monitors session state via ACP
2. On handoff request, exports session to shared storage
3. Mobile client imports session and continues
4. Desktop can re-import when returning

**Commands to leverage:**
```bash
# OpenCode
opencode export --session <id> --output session.json
opencode import session.json

# Generic (if agent supports ACP session/load)
acp session export <session-id> > session.json
acp session load session.json
```

### Option 3: Claude Code Web (Limited)

**Best for:** Claude Code users needing occasional mobile access

**Implementation:**
1. Desktop: `claude --remote` or `claude remote-control`
2. Mobile: Access claude.ai/code via ZeroClawed browser
3. **Limitation:** No return path to desktop terminal

---

## 7. References

### ACP Protocol
- [ACP Official Documentation](https://agentcommunicationprotocol.dev/)
- [ACP GitHub Repository](https://github.com/i-am-bee/acp)
- [ACP Python SDK](https://github.com/i-am-bee/acp/tree/main/python)
- [IBM ACP Technical Overview (WorkOS)](https://workos.com/blog/ibm-agent-communication-protocol-acp)
- [DeepLearning.AI ACP Course](https://learn.deeplearning.ai/courses/acp-agent-communication-protocol/information)
- [Agent Client Protocol (JetBrains/Zed)](https://agentclientprotocol.org/)

### Claude Code
- [Claude Code Remote Control Docs](https://code.claude.com/docs/en/remote-control)
- [Claude Code on the Web](https://code.claude.com/docs/en/claude-code-on-the-web)
- [Claude Code Session Management](https://code.claude.com/docs/en/sdk/sdk-sessions)

### OpenCode
- [OpenCode Server Documentation](https://opencode.ai/docs/server/)
- [OpenCode CLI Documentation](https://opencode.ai/docs/cli/)
- [OpenCode Web Documentation](https://opencode.ai/docs/web/)
- [OpenCode ACP Documentation](https://opencode.ai/docs/acp/)

### OpenClaw
- [OpenClaw ACP Agents Documentation](https://docs.openclaw.ai/tools/acp-agents)

### GitHub Issues
- [Gemini CLI Issue #15502 - Session Resume](https://github.com/google-gemini/gemini-cli/issues/15502)
- [OpenCode Issue #8931 - Session Management Commands](https://github.com/anomalyco/opencode/issues/8931)
- [OpenCode Issue #5445 - Attach with Session ID](https://github.com/sst/opencode/issues/5445)
- [OpenCode Issue #2168 - API Documentation](https://github.com/sst/opencode/issues/2168)

---

## Conclusion

### Executive Summary (Updated)

**Verdict: Highly feasible for OpenCode, limited for Claude Code.**

| Client | Handoff Support | ZeroClawed Integration |
|--------|----------------|---------------------|
| **OpenCode** | ✅ Native multi-client via HTTP server | ✅ Direct API proxy |
| **Claude Code** | ⚠️ One-way to web only | ❌ Closed proprietary system |
| **Generic ACP** | ❌ Not supported at protocol level | ⚠️ Requires proxy architecture |

### Recommended Implementation Path

**For immediate results:**
1. **Target OpenCode first** — its `serve` + `attach` commands provide exactly the session handoff capability needed
2. Use HTTP API proxying through ZeroClawed for mobile access
3. Leverage existing session persistence (SQLite in v1.2.0+)

**For Claude Code users:**
1. Document the `--remote` and `remote-control` workarounds
2. Clarify limitations (one-way, subscription required)
3. Consider session snapshot/restore as fallback

**Long-term:**
1. Build generic ACP session proxy for agents without native multi-client support
2. Standardize on session export/import format across agents
3. Explore WebSocket/SSE real-time sync for true simultaneous access

### Next Steps

1. **Prototype OpenCode integration:**
   ```bash
   opencode serve --port 4096 --hostname 0.0.0.0 --mdns
   # Test mobile access via ZeroClawed proxy
   ```

2. **Evaluate Tailscale Funnel** for secure mobile→desktop routing without VPN

3. **Investigate OpenCode's `/tui` endpoint** for programmatic TUI control

4. **Document Claude Code limitations** for users expecting full handoff
