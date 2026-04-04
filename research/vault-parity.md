# Vaultwarden + Bitwarden Agent Access SDK Parity

_Research date: 2026-03-30_

## What the Agent Access SDK Actually Uses

The Bitwarden Agent Access SDK (`github.com/bitwarden/agent-access`) is **not** a Bitwarden Vault REST API client. It operates on an entirely different protocol layer:

### Architecture Overview

The SDK establishes an **end-to-end encrypted tunnel using the Noise Protocol** (NNpsk2 pattern). There are two sides:

1. **User-client side** (`aac listen`) — runs locally on the user's machine alongside the Bitwarden CLI (`bw`). This side has vault access and handles credential approval requests.
2. **Remote client side** (`aac connect`) — runs in the agent/automation context. This side requests credentials from the user-client.

The tunnel connects via a **WebSocket proxy server** (`ap-proxy`) hosted at `wss://ap.lesspassword.dev` by default, or self-hosted. Neither side exposes vault APIs to the other.

### Protocol Layers

```
ap-error / ap-error-macro   — error handling utilities
ap-noise                    — multi-device Noise Protocol (NNpsk2 + PSK)
ap-proxy                    — zero-knowledge WebSocket rendezvous proxy  
ap-client                   — remote + user client implementations
ap-cli                      — `aac` CLI driver
```

### SDK API Surface (Python bindings example)

```python
from agent_access import RemoteClient

client = RemoteClient("python-remote")
client.connect(token="ABC-DEF-GHI")  # pairing token from `aac listen`
cred = client.request_credential("example.com")
print(cred.username, cred.password)
client.close()
```

The SDK communicates via the **Noise-encrypted tunnel**, not directly to any Bitwarden/Vaultwarden REST API.

### Pairing Flow

1. User runs `aac listen` locally (user-client mode)
2. The interactive CLI creates a pairing token
3. Agent uses `aac connect --token <pairing-token>` (or SDK `RemoteClient.connect()`)
4. Credential requests are routed through the proxy, decrypted on the user's machine
5. User approves the request; credential is returned encrypted through the tunnel

---

## Vaultwarden Parity Question

**The short answer: Vaultwarden parity is largely irrelevant for the Agent Access SDK.**

The SDK does not call Bitwarden's vault APIs at all. The credential provider (user-client side) can be:
- The official Bitwarden CLI (`bw`) — which does talk to Bitwarden/Vaultwarden REST APIs
- The built-in example provider (`aac listen --provider example`)
- Any custom credential provider the user implements

### What Vaultwarden Must Implement

For the **user-client side** to use Vaultwarden with `bw` CLI:

The Bitwarden CLI uses the [Bitwarden REST API](https://bitwarden.com/help/api/). Vaultwarden implements this API with high fidelity — it is specifically designed to be a drop-in replacement for the Bitwarden server. The key endpoints used by `bw` CLI:
- Vault unlock/login (`/identity/connect/token`)
- List items (`/api/cipher`)
- Get item by ID (`/api/ciphers/{id}`)
- Sync (`/api/sync`)

**Vaultwarden implements all of these.** The Vaultwarden project explicitly targets `bw` CLI compatibility as a core requirement.

### What Vaultwarden Does NOT Need to Implement

The Agent Access SDK **proxy** (`ap-proxy`) is a separate WebSocket rendezvous server that is *not* the Bitwarden server. Vaultwarden does not need to implement this. The user-client (`aac listen`) connects to a proxy that can be:
- Bitwarden's hosted proxy (`wss://ap.lesspassword.dev`)  
- A self-hosted `ap-proxy` instance

---

## Recommended Architecture for ZeroClawed

**Option A (Simplest): Use `aac` CLI as subprocess**
```
ZeroClawed → spawn `aac connect --domain <domain> --output json` → parse credential
```
- Works with both Bitwarden and Vaultwarden (user runs `bw` pointing at Vaultwarden)
- Approval is handled by user running `aac listen`
- No SDK embedding needed in Rust

**Option B: Use Rust SDK (crates)**
- The `ap-client` crate implements the remote client
- Dependency: requires linking the Noise protocol implementation
- Works with any credential provider, not tied to vault REST API at all
- More control, more complexity

**Option C: Bitwarden Official SDK (separate from Agent Access)**
- Bitwarden has a separate [sdk-internal](https://github.com/bitwarden/sdk-internal) Rust library
- This is for direct vault access (not the agent tunnel protocol)
- Vaultwarden would need to be compatible (it largely is for basic operations)

---

## Blockers / Unknowns

1. **`aac listen` approval UX is interactive/CLI**: the built-in approval flow requires a human sitting at a terminal running `aac listen`. This does not work for async/chat-based approval. ZeroClawed would need to either:
   - Implement its own credential provider (implement the `ap-client` user-client protocol)
   - Or bypass the tunnel entirely for its approval relay and use a different mechanism

2. **ap-proxy is required**: both sides need a reachable proxy. For self-hosted setups, this is one more service to run. For ZeroClawed, this could be bundled or pointed at Bitwarden's hosted proxy.

3. **Early preview / API instability**: The SDK README explicitly says "early preview stage, APIs and protocols are subject to change." Budget for API churn.

4. **Vaultwarden-specific SDK parity**: If ZeroClawed uses the separate Bitwarden Secrets Manager SDK (not Agent Access), Vaultwarden does NOT implement Secrets Manager APIs — only the personal vault APIs. This is a real gap if you want Vaultwarden + Secrets Manager pattern.

---

## Verdict for ZeroClawed

**Using Vaultwarden as the vault backend is viable** provided you use it via the `bw` CLI (which talks to Vaultwarden's standard vault API). The Agent Access SDK itself doesn't call vault APIs — it just encrypts a tunnel — so Vaultwarden parity for the tunnel is irrelevant.

**The main gap**: The Agent Access SDK's approval flow is CLI-interactive. ZeroClawed needs its own approval relay that bypasses or replaces the `aac listen` interactive model. This is the core design work needed.

---

## Sources

- `github.com/bitwarden/agent-access` — README + CONTRIBUTING.md + crate structure
- Bitwarden blog: "Introducing Agent Access SDK" (2026-03-30)
- OneCLI blog: "Bitwarden Integrates with OneCLI Agent Vault" (2026-03-30)
- Vaultwarden API compatibility: well-documented in Vaultwarden project docs (not directly fetched here but well-established)
