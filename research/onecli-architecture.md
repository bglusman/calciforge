# OneCLI Architecture Research

_Research date: 2026-03-30_
_Sources: github.com/onecli/onecli README, onecli.sh/blog posts_

---

## What OneCLI Is

OneCLI is an **HTTP gateway** (MITM proxy) that sits between AI agents and the external APIs they call. It intercepts outbound HTTP requests, pulls credentials from a vault, injects them into requests at the network layer, and forwards them. Agents never hold raw credentials.

**Core model:**
```
Agent → [sends fake/placeholder key] → OneCLI Gateway → [swaps in real credential] → External API
```

---

## Architecture Components

### 1. Rust Gateway (`apps/gateway`, port 10255)

The credential injection engine. Written in Rust.

- HTTP proxy with MITM interception (including HTTPS via certificate injection)
- Agents authenticate using `Proxy-Authorization` headers with access tokens
- Matches requests to credentials by **host pattern + path pattern**
- Decrypts credentials at request time only (AES-256-GCM)
- Applies rate limiting rules per-agent-identity per-host
- Fast + memory-safe — designed for high-throughput agent usage

**This is the piece that could be embedded or run alongside NZC/ZeroClawed.**

### 2. Web Dashboard (`apps/web`, port 10254)

Next.js app that provides:
- Management UI for agents, secrets, permissions
- REST API the gateway calls to resolve credential mappings
- Audit trail for requests
- Rate limit rule management

**Not Rust. Not embeddable.**

### 3. Secret Store

PostgreSQL-backed with AES-256-GCM encryption at rest. Secrets decrypted only at request time.

### 4. Vault Integration (Bitwarden and others)

OneCLI's Bitwarden integration works as follows:
- Configure Bitwarden as an "External Vault" provider in OneCLI dashboard
- When a request comes in needing a credential, OneCLI pulls it from Bitwarden via the Agent Access SDK
- Human approval through Bitwarden's approval flow is handled on the Bitwarden side
- OneCLI injects the approved credential into the proxied request

```bash
# Setup example
onecli provider add bitwarden --vault-url "https://vault.bitwarden.com"
onecli rules create --name "Stripe rate limit" \
  --host-pattern "api.stripe.com" \
  --action rate_limit \
  --rate-limit 10 \
  --rate-window 1h
```

---

## Deployment Model

### Quick Start (Docker Compose)

```bash
git clone https://github.com/onecli/onecli.git
cd onecli
docker compose -f docker/docker-compose.yml up
```

Everything starts as a single Docker stack: Rust gateway + Next.js app + PostgreSQL.

### Auth Modes

- **Single-user (no login)**: for local use (default)
- **Google OAuth**: for teams

---

## Is OneCLI a Library or a Sidecar?

**OneCLI is fundamentally a sidecar/service, not a library.**

### What this means for NZC/ZeroClawed integration:

1. **The Rust gateway binary is a standalone process** — it's not a crate you can embed in a Cargo workspace. The `apps/gateway` source is a full binary application, not a library crate.

2. **No published Rust crate** — OneCLI does not publish the gateway as a crate to crates.io. Integration requires running OneCLI as a separate Docker container or process.

3. **Dependency story for NZC**: NZC cannot link OneCLI's gateway as a Rust library dependency. The options are:
   - **Run OneCLI as a sidecar** alongside NZC (docker-compose or systemd)
   - **Implement the same proxy pattern natively** in NZC/ZeroClawed (build a credential-injecting HTTP proxy from scratch in Rust)
   - **Use OneCLI as a managed service** (their cloud offering at app.onecli.sh, or self-hosted)

4. **License**: Apache-2.0 — permissive, so the code can be referenced/adapted freely, but the architecture is designed as a service, not an embedded library.

---

## Relevance to ZeroClawed's Design

### What ZeroClawed Could Borrow Conceptually

OneCLI's architecture shows the **correct pattern** for agent credential injection:

1. Agent gets a fake/scoped token (not the real credential)
2. Agent sets `Proxy-Authorization: Bearer <fake-token>` on outbound HTTP
3. Gateway intercepts, looks up real credential by token + destination host
4. Gateway swaps in real credential, forwards request
5. No real credential ever touches agent memory/context

This pattern can be implemented natively in ZeroClawed/NZC without using OneCLI at all.

### What ZeroClawed Might Actually Use

For early implementation, running OneCLI as a sidecar alongside ZeroClawed is the **fastest path to get credential injection working**. The integration surface is small:
- Configure an agent's HTTP proxy to point at `localhost:10255`
- Set `Proxy-Authorization: Bearer <agent-token>` 
- Let OneCLI handle the rest

This does not require Rust embedding, library dependencies, or building a proxy from scratch.

### What's Missing from OneCLI for ZeroClawed's Use Case

OneCLI's current approval workflow (as of 2026-03-30) is:
- Rate limiting: **available now**
- Time-bound credentials: **on roadmap, not available**
- Human approval via chat (Signal/Telegram): **not available, not on roadmap**

OneCLI's blog post explicitly says "approval workflows" (pause + ask human) are "coming next" for high-risk actions — but the implementation will be through their own UI, not via Signal/Telegram. **ZeroClawed's chat-based approval relay is genuinely novel and not served by OneCLI.**

---

## Recommended Path

**Phase 1**: Use OneCLI as a sidecar for the basic credential injection + rate limiting layer. Quick to integrate, no Rust work needed.

**Phase 2**: ZeroClawed implements its own approval relay on top — when an agent requests a credential that requires human approval, ZeroClawed intercepts (either via OneCLI webhook/API or by implementing its own proxy) and sends an approval request to the user via Signal/Telegram. Once approved, credential is released.

**Phase 3** (if OneCLI sidecar is too heavy): implement the credential injection proxy natively in NZC using Rust, possibly adapting OneCLI's gateway approach. Apache-2.0 license makes this straightforward legally.

---

## NanoClaw Integration Context

The OneCLI blog confirms NanoClaw (25k stars, multi-agent framework) adopted OneCLI as their default credential layer. The pattern they use:
- Each NanoClaw **agent group** gets its own OneCLI vault identity
- Rate limits are set per identity per API endpoint
- Agents in the group use a shared access token scoped to that identity
- Human approval workflows (when they ship) will apply at the identity level

This is directly applicable to ZeroClawed's multi-claw design.

---

## Summary Table

| Question | Answer |
|----------|--------|
| Is OneCLI a library? | No — it's a service (sidecar/container) |
| Can NZC embed it as a Rust crate? | No — not published as a crate |
| Can NZC use it as a sidecar? | Yes — docker-compose or standalone binary |
| License? | Apache-2.0 (permissive) |
| Does it do chat-based approval? | No — not on roadmap |
| Does it support Vaultwarden? | Via Bitwarden integration (Vaultwarden is BW-compatible) |
| Is it production-ready? | Alpha-to-early-beta; NanoClaw adopted it in production |
