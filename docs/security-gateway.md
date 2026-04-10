# Security Gateway Architecture

The `security-gateway` is the mandatory network enforcement point for all ZeroClawed agent traffic. It replaces the legacy "opt-in" outpost model with a "fail-closed" transparent proxy.

## 🛡️ Traffic Flow

All outbound HTTP/HTTPS traffic from an agent is routed through the gateway.

**Outbound Pipeline:**
1. **Exfiltration Scan**: Outgoing request bodies are analyzed by the `adversary-detector` for secrets, PII, or adversarial patterns.
2. **Credential Injection**: The gateway detects the target API (e.g., OpenAI, Anthropic) and injects the required `Authorization` headers from the vault.
3. **Forwarding**: The request is forwarded to the destination.

**Inbound Pipeline:**
1. **Injection Scan**: Incoming response bodies are scanned for prompt injection or adversarial payloads.
2. **Enforcement**: If the response is deemed `unsafe`, the gateway blocks the content and returns a `403 Forbidden` to the agent.

## 🚀 Deployment & Enforcement

The gateway can be enforced at three tiers:

| Tier | Method | Level | Description |
|------|---------|--------|-------------|
| 1 | **Cooperative** | App | Set `HTTP_PROXY` / `HTTPS_PROXY` env vars. |
| 2 | **Enforced** | OS | `iptables` redirect of ports 80/443 to gateway. |
| 3 | **Isolated** | Net | Network namespaces restricting all traffic to the gateway. |

## ⚙️ Configuration

The gateway is configured via `GatewayConfig`:
- `scan_outbound`: Toggle exfiltration detection.
- `scan_inbound`: Toggle injection detection.
- `inject_credentials`: Toggle automatic API key injection.
- `bypass_domains`: List of domains that skip scanning (e.g., internal services).

## 🧪 Testing

Integration tests are located in `crates/security-gateway/tests/integration.rs`. They verify:
- Interception of adversarial content.
- Blocking of unsafe responses.
- Successful credential injection for known providers.
