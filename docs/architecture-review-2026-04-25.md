# Architecture Review - 2026-04-25

This review looks at repository-level architecture: crate boundaries, trust
boundaries, duplicated responsibilities, and the smallest set of moves that
would make the project easier to harden without slowing product work.

## Current Shape

The repository has six Rust crates:

| Crate | Current role |
|---|---|
| `calciforge` | Main channel router, agent adapter runtime, model proxy, local model manager, installer, voice path |
| `secrets-client` | Credential-injecting HTTP proxy and client library |
| `host-agent` | mTLS host RPC agent for ZFS/systemd/PCT/git/exec operations |
| `adversary-detector` | Prompt/content scanner, digest cache, HTTP scanning service |
| `security-proxy` | Outbound security gateway combining scanning and credential injection |
| `clashd` | Starlark policy sidecar with per-agent policy context |

The project direction is strong: security-sensitive capabilities are separated
into sidecars, and the host-agent has moved toward an adapter-first model. The
main architecture risk is not a missing crate; it is that multiple crates now
own overlapping versions of "policy", "proxy", "credential", and "gateway".

## Architectural Findings

### 1. There are too many policy planes without a shared decision contract

**Evidence:**

- `calciforge` has channel auth, routing rules, proxy access policy, and command
  handling.
- `clashd` evaluates Starlark tool policies.
- `host-agent` has its own approval policy, autonomy rules, rate limiting, and
  identity plugin.
- `adversary-detector` returns `Clean` / `Review` / `Unsafe` scan verdicts.
- `security-proxy` has gateway bypass rules, scanner decisions, and credential
  injection behavior.

Each piece is reasonable locally, but the system lacks one shared vocabulary
for a decision. That makes it hard to answer product/security questions such as:

- Which identity requested this?
- Which authority allowed or denied it?
- Was the result advisory, blocking, approval-gated, or audit-only?
- What stable audit ID ties the inbound request, policy decision, and downstream
  action together?

**Recommendation:**

Introduce a tiny shared "decision envelope" type before doing larger policy
rewrites. It can live in a small crate later, but can start as documentation and
one Rust module if that is faster:

```rust
pub struct DecisionContext {
    pub identity: String,
    pub channel: Option<String>,
    pub action: String,
    pub resource: Option<String>,
    pub request_id: String,
}

pub enum DecisionVerdict {
    Allow,
    Review { reason: String },
    Deny { reason: String },
}
```

Use this envelope at service boundaries first: channel command dispatch, proxy
authorization, host-agent `/host/op`, clashd `/evaluate`, and adversary-detector
scan results. The goal is not one giant policy engine; it is comparable
decisions and audit logs across engines.

### 2. `calciforge` is carrying too many runtime responsibilities

**Evidence:** `crates/calciforge/src` contains channel integrations, agent
adapters, command parsing, model proxy, provider/alloy routing, local model
server lifecycle, installer code, voice forwarding, persistent context, and
unified context.

This is still manageable, but it is approaching the point where every feature
appears to need a `calciforge` change. That increases merge conflicts and makes
security review harder because channel auth, model proxying, installer SSH, and
local shell hooks live in the same crate.

**Recommendation:**

Do not split everything immediately. Instead, draw three internal ownership
boundaries and enforce them with module APIs:

1. **Conversation runtime:** channels, identities, command parsing, routing,
   context.
2. **Model gateway:** OpenAI-compatible proxy, providers, alloys, local model
   lifecycle, voice passthrough.
3. **Installer/orchestration:** SSH, remote config patching, health checks,
   migration helpers.

Short term, add `README.md` or module docs under each boundary describing
allowed dependencies and owned config sections. Medium term, extract only the
model gateway if it keeps growing, because it already has an RFC and a coherent
separate lifecycle.

### 3. Credential proxy responsibilities are split three ways

**Evidence:**

- `secrets-client` injects credentials and also exposes direct vault lookup.
- `security-proxy` has credential injection concepts plus scanning.
- `calciforge` model proxy accepts configured provider headers and backend API
  keys.

The product goal is clear: agents should not directly hold API keys. The
implementation surface is less clear: there are multiple places where headers
or credentials can be introduced into an outbound request.

**Recommendation:**

Make OneCLI the only component that can turn a secret name into a provider
credential. Other components may pass a `SecretRef`, but should not read or
return the secret value. That would give a crisp rule:

> "Routers route. Scanners scan. OneCLI materializes credentials."

This also supports the fnox direction: a fnox writer or UI can create secret
material, but runtime callers still get only injection, not readback.

### 4. Host-agent wrapper hardening should become the default architecture

**Evidence:**

- `crates/host-agent/ADAPTERS.md` still describes adapters as calling broad
  privileged binaries such as `sudo -u <unix_user> zfs ...`, `sudo
  /usr/bin/systemctl ...`, and `sudo /usr/sbin/pct ...`.
- `docs/OPS-HARDENING.md` describes validated wrappers as the mitigation for
  broad sudo grants.

The wrapper model is the stronger architecture: Unix permissions remain the
enforcement layer, but the granted executable is intentionally narrow. The docs
currently present wrappers as hardening rather than the primary deployment
shape.

**Recommendation:**

Promote wrapper-only sudoers to the default host-agent architecture:

- Make adapter docs name the wrapper path for destructive or mutable commands.
- Keep read-only operations direct only where the sudoers entry is subcommand
  constrained.
- Add config validation that warns or fails when a mutable adapter is enabled
  without its wrapper path.
- Treat broad sudoers as an unsafe compatibility mode.

### 5. Model gateway primitives need one implementation path

**Evidence:** `docs/rfcs/model-gateway-primitives.md` is a strong design for
alloy/cascade/dispatcher, while current proxy/provider modules already have
alloy routing, fallback behavior, local model management, provider routing, and
model shortcuts.

The RFC gives the right vocabulary. The risk is implementing more incremental
routing behavior before the primitive model lands, creating several partial
ways to solve the same problem.

**Recommendation:**

Use the RFC as the gateway roadmap and resist new ad hoc model-routing concepts:

1. Add the shared token-estimator trait and request-size calculation.
2. Harden existing alloys with context-window validation.
3. Promote existing fallback behavior into explicit cascades.
4. Add dispatchers for "smallest sufficient model" routing.

This sequencing keeps existing behavior working while moving toward a coherent
gateway architecture.

## Minimal Next Moves

1. Document the shared decision envelope and add request IDs to all major
   security decisions.
2. Make OneCLI the only runtime secret materializer; deprecate plaintext secret
   readback paths.
3. Update host-agent docs/config toward wrapper-first deployment.
4. Implement the model gateway RFC in small vertical slices instead of adding
   more bespoke routing features.
5. Add boundary docs for the three `calciforge` subdomains before extracting
   crates.

## Non-Goals

- Do not collapse everything into one central policy engine.
- Do not split every module into a new crate just to make the dependency graph
  look tidy.
- Do not couple the open-source architecture to any one operator's deployment
  topology.

The project is already close to a good shape. The highest-leverage improvement
is to make the contracts between the sidecars explicit, especially identity,
decision, audit, and credential-materialization contracts.
