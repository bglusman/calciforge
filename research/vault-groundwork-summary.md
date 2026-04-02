# Vault + PolyClaw Groundwork — Research Summary

_Date: 2026-03-30_
_Covers: vault-integration-plan.md Workstreams 1, 2, and 3_

---

## TL;DR

Three workstreams, three different readiness levels:

| Workstream | Topic | Status | First Milestone |
|------------|-------|--------|-----------------|
| 1 | Vault Integration | ✅ Feasible now | `EnvAdapter` + approval relay skeleton |
| 2 | PolyClaw OpenClaw Adapter | ⚠️ Design work needed | Version-gated safe installer |
| 3 | OpenClaw Migration + Fallback | ✅ Partially feasible now | Chat completions pass-through |

---

## Workstream 1: Vault Integration

### What We Found

**Bitwarden Agent Access SDK** is not a vault REST API client. It's an end-to-end Noise-protocol encrypted tunnel. Two sides:
- *User-client* (`aac listen`): runs locally, has vault access, handles approval interactively
- *Remote client* (`aac connect` / SDK): requests credentials over the tunnel

The SDK does not call Bitwarden's vault APIs directly. Vaultwarden compatibility is therefore **not the right question** — Vaultwarden runs fine with `bw` CLI which is what the user-client uses.

**The real gap**: the SDK's approval flow is interactive CLI. It does not support async/chat-based approval. PolyClaw must build its own approval relay.

**OneCLI** is a sidecar service (not an embeddable Rust crate) that sits in front of outbound HTTP calls and injects credentials at the network layer. Apache-2.0 licensed. Currently does rate limiting; async approval workflows are on their roadmap but not available. Good reference architecture and usable as a sidecar today.

### What's Feasible Now

- **`EnvAdapter`** (reads from environment variables) — can be built with zero external dependencies
- **`VaultwardenAdapter`** (direct API via `bw` CLI subprocess) — works today with Vaultwarden since it implements the standard Bitwarden REST API
- **Approval relay skeleton** — the state machine, message formatting, and channel delivery are all implementable now; the vault fetch after approval is a one-liner via `bw` CLI

### What Needs Design Work

- **`BitwardenAdapter` using Agent Access SDK**: SDK is early preview, API unstable. The tunnel protocol is in Rust though (`ap-client` crate) — could be integrated as a Cargo dependency when stable.
- **Approval granularity policy**: per-use vs per-session vs time-bound — design decision needed before implementation
- **OneCLI integration for "approval before inject"**: when OneCLI gates a request, it currently doesn't call back to NZC. Either implement Option A (catch 403, trigger relay, retry) or wait for OneCLI to add callback support.

### Suggested First Milestone

> **Milestone V1**: `EnvAdapter` + `VaultwardenAdapter` (via `bw` subprocess) + approval relay that sends Telegram/Signal message and waits for `/approve` reply.

No external SDK dependency. Works end-to-end. Replaces `~/credentials/` plaintext files with vault-backed access + human approval for sensitive operations.

---

## Workstream 2: PolyClaw OpenClaw Adapter

### What We Found

**No native PolyClaw channel type exists** in OpenClaw's schema. The integration options, in order of risk:

1. **Chat completions proxy** (`POST /v1/chat/completions`) — works today, confirmed on live instance, requires only one config field to enable
2. **Hooks ingress** (`POST /hooks/agent`) — requires `hooks.*` config block, lower risk than channel additions
3. **Native plugin channel** (`channels.polyclaw`) — best long-term architecture, highest install risk

**Schema versioning**: There's no `openclaw config schema` command or HTTP schema endpoint. Version is detectable via `openclaw --version` (SSH) or `meta.lastTouchedVersion` in `openclaw.json`. PolyClaw must maintain its own compatibility matrix.

**The 2026-03-30 incident** was caused by adding a channel config entry without installing the corresponding plugin first. The fix is the proposed safe installer flow: backup → version check → matrix lookup → doctor validate → confirm → apply → health check → rollback.

### What's Feasible Now

- **Pass-through via chat completions**: one field to enable, works today
- **Safe installer skeleton**: the version detection + backup + doctor validate loop can be written now
- **Compatibility matrix for 2026.3.x**: we know exactly which fields are safe to add without plugins

### What Needs Design Work

- **PolyClaw plugin**: the TypeScript plugin that registers `channels.polyclaw` — needs to be written and packaged
- **Full installer with rollback**: needs SSH access to target instance to run `openclaw doctor` and `gateway restart`
- **Multi-instance targeting**: installer needs to handle "I have 3 claws, apply to all" safely (one at a time!)

### Suggested First Milestone

> **Milestone A1**: Safe pass-through adapter — PolyClaw enables `chatCompletions` endpoint on a target claw (with backup + version check + confirm), then relays messages via `/v1/chat/completions`. No plugin installation required.

---

## Workstream 3: OpenClaw Migration + Fallback

### What We Found

**OpenClaw data format**:
- Config: `~/.openclaw/openclaw.json` (JSON5, human-readable)
- Sessions: JSONL append-only event logs per session, flat files, no SQLite
- Memory: plain Markdown files in the agent workspace (`MEMORY.md`, `memory/YYYY-MM-DD.md`)
- Session index: `sessions.json` — flat JSON object, 508 entries on live instance

**Pass-through feasibility**: Confirmed working. `POST /v1/chat/completions` with `"user": "<stable-id>"` gives session continuity. Tested live on Librarian (2026-03-30).

**Memory migration**: Trivial. It's Markdown files. Copy them verbatim. NZC can read them directly if it supports a similar workspace concept.

**Session migration**: Moderate effort. JSONL event log format with `id`/`parentId` DAG. Conversion to NZC's format needed, but schema is clear.

**WhatsApp session migration**: Not feasible. Baileys auth state is non-portable.

### What's Feasible Now

- **Workspace memory import**: copy `MEMORY.md` + `memory/*.md` verbatim — zero engineering
- **Pass-through fallback adapter**: one HTTP client struct, ~100 lines of Rust
- **Config field mapping tool**: read `openclaw.json`, output NZC-equivalent config as a diff — moderate effort

### What Needs Design Work

- **Session history import**: JSONL → NZC format converter; decide how many months of history to offer
- **Fallback session continuity**: implement session key pinning (`x-openclaw-session-key` header) so multi-turn conversations work through the fallback
- **Fallback UX**: should users see "⚡ via OpenClaw" indicator? Design decision.
- **WhatsApp migration path**: probably just "re-scan QR code in NZC" — no technical migration possible

### Suggested First Milestone

> **Milestone M1**: NZC installer detects existing OpenClaw installation, offers to copy workspace memory files, generates NZC config diff from `openclaw.json`, and configures a pass-through fallback to the existing OpenClaw gateway. User approves each step explicitly.

---

## Blocker Map

| Blocker | Severity | Affects | Mitigation |
|---------|----------|---------|------------|
| Agent Access SDK is early preview, API unstable | Medium | Workstream 1 (BitwardenAdapter) | Use `bw` CLI subprocess instead; watch SDK for stable release |
| No OpenClaw HTTP schema endpoint | Medium | Workstream 2 | Maintain compatibility matrix; rely on `doctor` for validation |
| OneCLI async approval not available | Low | Workstream 1 (OneCLI integration) | Use retry-after-403 pattern instead; OneCLI callback on roadmap |
| `channels.polyclaw` needs plugin installed first | High | Workstream 2 (native channel) | Use chat completions pass-through for Phase 1; plugin is Phase 3 |
| WA session not portable | Low | Workstream 3 | Document: WA users must re-pair; this is expected and unavoidable |
| OpenClaw version not exposed via HTTP | Low | Workstream 2, 3 | Use `meta.lastTouchedVersion` in config; SSH for accurate version |

---

## Cross-Cutting: Credential Flow During Install

The planning doc notes these workstreams connect: when the PolyClaw adapter installer generates credentials (e.g. `hooks.token`, shared secret between PolyClaw and the claw), those credentials should go into vault, not written to a config file.

**Recommended flow**:
```
1. Installer generates hooks.token (random, 32 bytes)
2. Store token in vault via VaultAdapter.store_secret("openclaw/<hostname>/hooks-token", ...)
3. Write token to openclaw.json as SecretRef: { source: "exec", provider: "vault", id: "openclaw/<hostname>/hooks-token" }
4. Token never appears in plaintext in openclaw.json
```

This requires the vault integration (Workstream 1) to be working before the safe installer (Workstream 2) can operate at full security. But they can be developed in parallel — just use `EnvAdapter` during installer development and swap in vault later.

---

## Recommended Implementation Order

```
Week 1-2: Foundation
  ├── EnvAdapter (trivial, no deps)
  ├── VaultwardenAdapter (bw CLI subprocess)  
  ├── ApprovalRelay state machine (in-process, Telegram/Signal delivery)
  └── OpenClaw pass-through adapter (chat completions HTTP client)

Week 3-4: Installer MVP
  ├── Version detection (SSH + config file fallback)
  ├── Compatibility matrix for 2026.3.x
  ├── Safe install flow (backup → validate → confirm → apply → health check → rollback)
  └── Enable chatCompletions + hooks.enabled (minimal footprint, low risk)

Week 5-6: Migration Tool
  ├── Workspace memory copy
  ├── openclaw.json → NZC config diff generator
  └── Fallback session continuity (session key pinning)

Later: Phase 2/3
  ├── PolyClaw plugin (native channels.polyclaw)
  ├── BitwardenAdapter (Agent Access SDK, when stable)
  └── Full session history import
```

---

## Files in This Research Directory

| File | Contents |
|------|----------|
| `vault-parity.md` | Agent Access SDK architecture, Vaultwarden compat analysis |
| `onecli-architecture.md` | OneCLI as sidecar/service, embedding story, NZC integration path |
| `openclaw-integration-points.md` | Four integration options (completions, hooks, plugin, tools/invoke) |
| `openclaw-schema-versioning.md` | Version detection, schema gaps, compatibility matrix approach |
| `approval-relay-design.md` | Full async approval flow, state machine, Rust interface, UX design |
| `openclaw-migration.md` | Data directory structure, session format, HTTP API, migration field map |
| `vault-groundwork-summary.md` | This file |
