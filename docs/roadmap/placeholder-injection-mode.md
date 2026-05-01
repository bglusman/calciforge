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
- **Combine with current {{secret:NAME}} mode.** Both can coexist —
  agents that know about Calciforge use the explicit syntax, agents
  that don't get placeholder injection. Recognizer scans for both
  patterns.

## Out of scope for first cut

- Kernel-level enforcement (would require eBPF / dyld interposition)
- Process-level sandbox (chroot / namespace) to prevent the agent
  from seeing real env from the parent — useful but separate
- Per-call placeholder rotation (mostly noise; per-agent is enough)

## Rough effort estimate

~1 week for a working prototype:
- 2 days: PlaceholderMap data structure + lifecycle hooks in security-proxy
- 2 days: spawn integration in calciforge router (which agents get which placeholders)
- 1 day: recognizer + substitution in proxy hot path
- 1 day: tests + docs

Compared to the months that a true eBPF implementation would take
(plus the ongoing Linux-only constraint), the cost-benefit strongly
favors placeholder injection unless someone shows up needing kernel-
enforced isolation specifically.
