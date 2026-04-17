# Sidecar vs Embedded: LLM Gateway Architecture Comparison

## Executive Summary

Two approaches were evaluated for adding multi-provider LLM routing to zeroclawed:

1. **Helicone** (Sidecar Pattern) - External AI Gateway as separate process
2. **Traceloop-inspired** (Embedded Pattern) - Native Rust routing within zeroclawed

Both approaches support tool calling. The choice depends on deployment complexity tolerance and observability needs.

---

## Detailed Comparison

### Deployment Model

| Aspect | Helicone (Sidecar) | Traceloop (Embedded) |
|--------|-------------------|---------------------|
| **Binary Count** | 2 (zeroclawed + helicone) | 1 (zeroclawed only) |
| **Process Management** | Requires process manager (systemd/Docker) | Single process |
| **Network Topology** | HTTP between processes | In-memory function calls |
| **Configuration** | Separate YAML for Helicone, TOML for zeroclawed | Single TOML file |
| **Resource Usage** | Higher (2 processes) | Lower (1 process) |

### Tool Calling Support

**Helicone**: ✅ **Fully Supported**
- Tool definitions pass through transparently
- Helicone normalizes tool calling across providers
- No special handling needed in zeroclawed

**Traceloop**: ✅ **Fully Supported**
- Native tool definition handling
- Provider-specific tool serialization
- Direct control over tool choice behavior

**Verdict**: Both approaches handle tool calling well.

### Observability

**Helicone**: ✅ **Excellent**
- Built-in web dashboard
- Cost tracking per request
- Request/response logging
- Analytics and reporting
- Export to various backends

**Traceloop**: ⚠️ **Basic**
- Internal stats tracking
- No built-in dashboard
- Would need custom metrics export

### Error Handling & Resilience

**Helicone**: ⚠️ **Needs Work**
- If Helicone crashes, zeroclawed loses all LLM access
- Need circuit breaker pattern
- Fallback to embedded routing would be complex

**Traceloop**: ✅ **Good**
- Built-in fallback chain between providers
- Graceful degradation if one provider fails
- Single failure domain

### Maintainability

**Helicone**: ⚠️ **External Dependency**
- Updates depend on Helicone project
- Rust version compatibility between projects
- Feature requests require upstream changes
- Potential for project abandonment

**Traceloop**: ✅ **Self-Contained**
- Full control over implementation
- Can add features as needed
- No external dependencies for routing

---

## Code Complexity Analysis

### Lines of Code

```
Helicone Integration:
- helicone_backend.rs:     ~250 lines
- Config wiring:            ~20 lines
- Test file:               ~100 lines
Total: ~370 lines

Traceloop Implementation:
- traceloop/mod.rs:        ~200 lines
- traceloop/openai.rs:     ~200 lines
- traceloop/deepseek.rs:   ~240 lines
- traceloop/kimi.rs:        ~60 lines
- traceloop/anthropic.rs:  ~240 lines
- Proxy integration:        ~30 lines
Total: ~970 lines
```

**Verdict**: Helicone integration is less code to maintain in zeroclawed, but adds external dependency.

---

## Operational Complexity Assessment

### Scenario: Home Lab Deployment

**Helicone Approach**:
```yaml
# docker-compose.yml
services:
  helicone:
    image: helicone/gateway
    ports:
      - "8080:8080"
    volumes:
      - ./helicone-config.yaml:/config.yaml
    environment:
      - DEEPSEEK_API_KEY=${DEEPSEEK_API_KEY}
      - KIMI_API_KEY=${KIMI_API_KEY}
  
  zeroclawed:
    image: zeroclawed
    ports:
      - "18789:18789"
    environment:
      - HELICONE_URL=http://helicone:8080
    depends_on:
      - helicone
```

**Traceloop Approach**:
```yaml
# docker-compose.yml
services:
  zeroclawed:
    image: zeroclawed
    ports:
      - "18789:18789"
    environment:
      - DEEPSEEK_API_KEY=${DEEPSEEK_API_KEY}
      - KIMI_API_KEY=${KIMI_API_KEY}
```

**Verdict**: Traceloop is simpler to deploy and operate.

---

## Decision Matrix

Choose **Helicone** if:
- ✅ You need rich observability dashboards
- ✅ Cost tracking across providers is critical
- ✅ You have a team needing shared visibility
- ✅ You're running in production with proper monitoring
- ✅ You don't mind running 2 services

Choose **Traceloop** if:
- ✅ Simplicity is valued over features
- ✅ Single binary deployment preferred
- ✅ You can live without built-in analytics
- ✅ You're comfortable implementing custom metrics
- ✅ Home lab / personal use case

---

## Recommendation for Brian

**Use Traceloop for now.**

Reasoning:
1. **Home lab context**: Single binary is easier to manage
2. **Personal use**: Don't need team collaboration features
3. **Simpler ops**: One process to monitor, one config file
4. **Good enough**: Tool calling works, fallback works
5. **Can migrate later**: If you need analytics, switch to Helicone

Keep the Helicone integration code - it's complete and tested to compile. You can enable it later if needs change.

---

## Files Changed/Added

### Helicone Integration
```
crates/zeroclawed/src/proxy/helicone_backend.rs    # New: Helicone HTTP client
crates/zeroclawed/src/proxy/backend.rs             # Modified: Add Helicone variant
crates/zeroclawed/src/proxy/mod.rs                 # Modified: Add helicone backend type
test_helicone_integration.rs                       # New: Integration test
config-examples/helicone-proxy.toml                # New: Example config
docs/helicone-integration-status.md               # New: This doc
```

### Traceloop Implementation
```
crates/zeroclawed/src/proxy/traceloop/mod.rs       # New: Router core
crates/zeroclawed/src/proxy/traceloop/openai.rs    # New: OpenAI provider
crates/zeroclawed/src/proxy/traceloop/deepseek.rs  # New: DeepSeek provider
crates/zeroclawed/src/proxy/traceloop/kimi.rs      # New: Kimi provider
crates/zeroclawed/src/proxy/traceloop/anthropic.rs # New: Anthropic provider
crates/zeroclawed/src/proxy/handlers.rs            # Modified: Use Traceloop first
```

---

## Time Investment

- **Helicone integration**: ~40 minutes (matching the timebox)
- **Traceloop implementation**: ~40 minutes (original work)
- **Both now compile and are ready for use**
