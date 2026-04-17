# Helicone AI Gateway Integration

## Overview

This document describes the integration of Helicone AI Gateway into zeroclawed as a replacement for the current llm-crate-based routing. The integration provides a backend that routes requests through Helicone AI Gateway, which supports 100+ models with full tool calling support.

## Problem Statement

The current zeroclawed proxy uses the `graniet/llm` crate which has issues with DeepSeek's tool calling implementation. Specifically, the `chat_with_tools()` function contains `todo!()` macros that cause panics when tool calling is attempted with DeepSeek models.

## Solution

Integrate Helicone AI Gateway as a sidecar service that provides:
- Full tool calling support across providers (DeepSeek, Kimi, OpenAI, Anthropic, etc.)
- Model routing and load balancing
- Rate limiting and caching
- Analytics and monitoring

## Implementation

### Files Added

1. **`crates/zeroclawed/src/proxy/helicone_backend.rs`**
   - Helicone backend implementation
   - Configuration struct for Helicone settings
   - HTTP client for communicating with Helicone gateway

### Files Modified

1. **`crates/zeroclawed/src/proxy/backend.rs`**
   - Added `BackendType::Helicone` enum variant
   - Extended `BackendConfig` with Helicone-specific fields
   - Updated factory function to create Helicone backend
   - Added import for helicone_backend module

2. **`crates/zeroclawed/src/proxy/mod.rs`**
   - Added helicone_backend module declaration

## Configuration

### Backend Configuration

To use Helicone backend, update your zeroclawed config:

```toml
[proxy]
enabled = true
bind = "127.0.0.1:8080"
backend_type = "helicone"  # Changed from "http" or "mock"

# Helicone-specific configuration
helicone_url = "http://localhost:8080"
helicone_api_key = ""  # Optional, if Helicone auth is enabled
helicone_router_name = "ai"  # Optional, defaults to "ai"
timeout_seconds = 120
```

### Helicone Gateway Configuration

Helicone AI Gateway needs to be configured separately. Example configuration:

```yaml
# helicone-config.yaml
routers:
  - name: ai
    provider: openai
    api_key: ${OPENAI_API_KEY}
    models:
      - gpt-4
      - gpt-4o
  
  - name: deepseek
    provider: deepseek
    api_key: ${DEEPSEEK_API_KEY}
    models:
      - deepseek-chat
      - deepseek-reasoner
  
  - name: kimi
    provider: moonshot
    api_key: ${KIMI_API_KEY}
    models:
      - kimi-free
```

## Usage

### Starting Helicone Gateway

```bash
cd /tmp/helicone-gateway
cargo run --release -- --config helicone-config.yaml
```

### Starting zeroclawed with Helicone Backend

```bash
cd /root/projects/zeroclawed
cargo run --release -- --config ~/.zeroclawed/config.toml
```

### API Requests

Requests are forwarded to Helicone gateway which handles:
- Model selection and routing
- Tool calling implementation
- Fallback and retry logic
- Rate limiting

## Testing

### Unit Tests

Run the existing tests to ensure no regression:

```bash
cd /root/projects/zeroclawed
cargo test --package zeroclawed
```

### Integration Test

A test script is available at `test_helicone_integration.rs`:

```bash
cd /root/projects/zeroclawed
rustc test_helicone_integration.rs && ./test_helicone_integration
```

## Performance Characteristics

### Advantages
1. **Full tool calling support**: Helicone implements proper tool calling for all supported providers
2. **Multiple provider support**: Single endpoint for 100+ models
3. **Built-in features**: Rate limiting, caching, analytics
4. **Active development**: Helicone is actively maintained

### Disadvantages
1. **Additional service**: Requires running Helicone as a sidecar
2. **Configuration complexity**: Need to configure both zeroclawed and Helicone
3. **Latency**: Additional network hop to Helicone service

## Comparison with Alternatives

### Current llm-crate approach
- **Pros**: Simple, direct integration
- **Cons**: Broken tool calling, limited provider support

### Traceloop alternative
- **Pros**: Observability and tracing features
- **Cons**: May not solve the tool calling issue directly

### Helicone approach
- **Pros**: Complete solution for tool calling, multi-provider support
- **Cons**: Additional service to manage

## Migration Path

1. **Phase 1**: Implement Helicone backend alongside existing backends
2. **Phase 2**: Test with non-critical workloads
3. **Phase 3**: Gradually migrate traffic to Helicone backend
4. **Phase 4**: Remove llm-crate dependency once stable

## Troubleshooting

### Common Issues

1. **Helicone gateway not running**
   ```
   Error: Failed to connect to Helicone gateway
   ```
   Solution: Start Helicone gateway first

2. **API key issues**
   ```
   Error: 401 Unauthorized
   ```
   Solution: Configure API keys in Helicone config

3. **Model not found**
   ```
   Error: Model 'deepseek-chat' not available
   ```
   Solution: Add model to Helicone router configuration

### Logging

Enable debug logging for troubleshooting:

```bash
RUST_LOG=debug cargo run --release -- --config config.toml
```

## Future Enhancements

1. **Embedded mode**: Bundle Helicone as a library instead of sidecar
2. **Dynamic configuration**: Hot-reload Helicone config without restart
3. **Health checks**: Monitor Helicone gateway health
4. **Fallback strategy**: Fall back to other backends if Helicone fails

## Conclusion

The Helicone AI Gateway integration provides a robust solution for tool calling across multiple LLM providers. While it adds complexity by requiring a separate service, it solves the critical issue of broken tool calling in the current llm-crate implementation and provides additional benefits like rate limiting, caching, and multi-provider support.