---
layout: default
title: Placeholder Injection Mode
---

# Placeholder-injection mode (Kloak-inspired transparency)

Status: ROADMAP — captured 2026-04-25 from a discussion of
[Kloak](https://getkloak.io/)'s eBPF kernel-level interception
approach.

## Background

Kloak (Kubernetes eBPF HTTPS interceptor) achieves a strong property:
*the application never holds real secrets*. The app holds opaque
placeholder tokens; an eBPF program in the kernel TLS path swaps
placeholders for real credentials at the moment of network send. If
the app process is compromised and its memory is dumped, the attacker
gets placeholders, not credentials.

We can't trivially port their mechanism — eBPF is Linux-only, requires
root + recent kernels, and TLS interception specifically requires
uprobes into userspace TLS libraries. Calciforge runs on macOS as a
first-class target; we'd be cutting that off.

## Proposed approach: HTTP-proxy-level placeholder injection

The same property — "agent never holds real secret" — can be achieved
with our existing HTTP-proxy architecture by inverting the current
flow:

### Current flow (`{{secret:NAME}}` substitution)
1. Agent author writes `Authorization: Bearer {{secret:OPENAI_KEY}}`
2. Agent process emits that literal string in its request
3. security-proxy substitutes `{{secret:OPENAI_KEY}}` → real value
4. Real value goes to upstream

**Property:** agent never sees the real value, BUT must know about the
substitution syntax. Off-the-shelf agents that don't know about
Calciforge can't use this — they hardcode env-var reads.

### Proposed flow (placeholder injection)
1. Calciforge spawns the agent process with env:
   `OPENAI_API_KEY=cfg_OPENAI_KEY_a1b2c3d4e5f6...` (per-agent random)
2. Agent reads env, thinks it holds the real key, and emits an
   `Authorization: Bearer …` header carrying the placeholder value
3. security-proxy recognizes the placeholder pattern,
   looks up real value in its per-agent placeholder→secret map,
   substitutes, forwards to upstream
4. Real value goes to upstream

**Property:** agent never sees the real value AND doesn't need to know
about Calciforge. Works with any off-the-shelf agent that reads
credentials from env vars.

## Current staged implementation

Status as of 2026-05-12: the security-proxy-local primitives are merged, but
live placeholder substitution is intentionally not enabled yet.

Implemented pieces:
- Placeholder token recognition for `cfg_<NAME>_<32-hex>`.
- Placeholder rendering keyed by the full opaque token, not by the
  embedded name hint.
- Placeholder token generation from validated secret names.
- Per-agent `PlaceholderMap` that resolves `agent_id + token` to an
  authoritative secret name.
- Fail-closed token-set resolution when any discovered placeholder is
  not registered for the current agent.
- Security-proxy lifecycle helpers to register generated placeholders,
  generate-and-register a placeholder in one step, retire one
  placeholder, or retire all placeholders for an agent.
- A shared identity/destination policy gate that placeholder
  substitution can reuse before any real secret value is loaded.
- An inert `SecurityProxy` placeholder-name resolution helper that
  scans request text, resolves token -> secret name, and applies the
  same policy gate as explicit `{{secret:NAME}}` references.

Not yet wired:
- A runtime source that calls the lifecycle helpers from supervised
  agent spawn/shutdown or equivalent channel lifecycle code.
- Passing generated placeholders into agent env vars, wrapper files, or
  managed credential directories at spawn/install time.
- Live request rewriting from placeholder token -> real secret value.

The next safe implementation slice is Calciforge-side lifecycle wiring:
decide which supervised agent runtime owns placeholder creation, where
generated values are injected, and where single-token or whole-agent
retirement is called. That delivery surface may be an env var for CLI agents,
a wrapper-generated config file, or a managed credentials folder for agents
that already expect plaintext files. Live substitution should remain disabled
until that owner can deterministically register and retire tokens.

That boundary is explicit: `calciforge` currently owns agent config and
adapter construction, while `security-proxy` owns the in-memory
placeholder registry. `AgentConfig.env` is cloned into concrete
subprocess adapters at adapter construction time, and installer-managed
wrappers separately export `CALCIFORGE_AGENT_ID` for central secret
helper calls. Do not hide placeholder generation inside an adapter
constructor by making adapters reach into `SecurityProxy`; that would
make lifecycle ownership and retirement ambiguous. The next
implementation should either move the placeholder lifecycle API into a
shared crate used by both sides, or add an explicit Calciforge-owned
registration channel/client before adapter env maps are rewritten.

## What we'd build

Per-agent state in security-proxy:
```rust
pub struct PlaceholderMap {
    /// agent_id → placeholder token → real secret name
    /// e.g. "claude-research" → "cfg_OPENAI_KEY_a1b2..." → "OPENAI_API_KEY"
    by_agent: HashMap<String, HashMap<String, String>>,
}
```

When Calciforge starts an agent (today: the channel router does this
indirectly via the openclaw adapter; tomorrow: explicit "spawn under
supervision" entry point):
1. Look up which secrets the agent's config references
2. Generate a per-agent random placeholder for each
3. Set agent's env to use placeholders
4. Register placeholders in security-proxy's PlaceholderMap

When security-proxy sees an outbound request, it scans body + headers
for placeholder shapes (regex on the `cfg_*_*` prefix) and swaps
through PlaceholderMap before forwarding. Same code path as
`{{secret:NAME}}` substitution — just a different recognizer.

Important invariant: the placeholder path must never trust the
embedded `<NAME>` hint in `cfg_<NAME>_<random>`. It must resolve the
full opaque token through the per-agent map, apply the same
per-agent/user/channel secret access policy and destination allowlist
as explicit `{{secret:NAME}}` substitution, and only then load the real
secret value.

## Comparison vs. true eBPF interception

| Property | True eBPF | Placeholder injection |
|---|---|---|
| Agent never sees real secret | ✅ | ✅ |
| Works without agent's awareness of Calciforge | ✅ | ✅ |
| Kernel-enforced (agent can't bypass) | ✅ | ❌ (agent can bypass cooperative proxy env unless paired with host/container egress controls) |
| Linux only | yes | no — cross-platform |
| Requires root | yes (CAP_BPF) | no |
| Requires recent kernel | yes (5.x+) | no |
| Engineering cost | months | ~1 week |
| Debuggability | brutal | normal HTTP-proxy logs |
| Compatible with our existing substitution engine | rewrite | direct extension |

## Threat model deltas

**Things both approaches catch:**
- Agent process memory dumped by attacker → placeholder, not secret
- Agent log lines accidentally include the credential → placeholder
- Agent uploads its own env var to an untrusted endpoint → placeholder

**Things only true eBPF catches:**
- Agent intentionally bypasses cooperative proxy env to talk directly →
  placeholder is useless because no upstream knows what it means. Pair this
  mode with host/container egress controls when bypass resistance matters.

**Things neither catches:**
- Agent intentionally exfiltrates the placeholder + asks Calciforge
  to send a request through (Calciforge will substitute and the real
  value goes to attacker via a different gated request). The
  destination-allowlist (RFC §11.1) we already shipped is the defense
  for this — placeholder injection alone doesn't help.

## Implementation notes

- **Placeholder shape matters.** Should be unique enough to be
  recognized cheaply (regex on a known prefix), random enough to not
  collide with anything real, AND not look like a real secret to
  scanners. Suggest: `cfg_<NAME>_<32-hex>` — 36 chars + name length.
- **Placeholder lifecycle.** Generate at agent spawn, retire at agent
  shutdown. If the agent restarts under Calciforge supervision, new
  placeholders. If the agent persists secrets to disk and reads them
  back, the placeholder must persist too (or rotate-on-restart breaks
  the agent).
- **Multiple agents, same secret name.** Two agents both wanting
  `OPENAI_API_KEY` get different placeholders pointing to the same
  real value. Keeps per-agent isolation.
- **Lifecycle boundary.** Calciforge-side code must own the decision to
  generate and inject placeholder-backed env because it owns
  `AgentConfig.env` and supervised process construction. Security-proxy
  must own the authoritative token registry and value-substitution
  policy. A shared lifecycle API or explicit registration client should
  connect those two responsibilities.
- **Combine with current {{secret:NAME}} mode.** Both can coexist
  indefinitely. Operators may prefer explicit references for agent-aware
  workflows because they are easy to audit, and placeholder injection for
  ordinary tools that only understand env vars or credential files. Recognizer
  scans for both patterns.
- **Coordinate with exfil scanners.** Opaque placeholders are intentionally
  random and may look like real API keys to IronClaw-style exfil detection.
  Until scanners can consult the placeholder registry or an equivalent
  allowlist, operators may need to choose between strict exfil scanning and
  placeholder injection for a given runtime.

## Out of scope for first cut

- Kernel-level enforcement (would require eBPF / dyld interposition)
- Process-level sandbox (chroot / namespace) to prevent the agent
  from seeing real env from the parent — useful but separate
- Per-call placeholder rotation (mostly noise; per-agent is enough)

## Rough effort estimate

~1 week for a working prototype:
- 2 days: PlaceholderMap data structure + lifecycle hooks in security-proxy
- 1 day: define shared lifecycle API or explicit registration client between Calciforge and security-proxy
- 2 days: spawn integration in calciforge router/adapters (which agents get which placeholders)
- 1 day: recognizer + substitution in proxy hot path
- 1 day: tests + docs

Compared to the months that a true eBPF implementation would take
(plus the ongoing Linux-only constraint), the cost-benefit strongly
favors placeholder injection unless someone shows up needing kernel-
enforced isolation specifically.
