# Vendor zeroclawlabs 0.6.7 — Comparison Report

**Date:** 2026-03-31  
**Upstream:** `zeroclawlabs` crate v0.6.7, published 2026-03-29 on crates.io  
**Upstream repo:** `github.com/zeroclaw-labs/zeroclaw` — **DELETED** (404 as of 2026-03-30)  
**Our crate:** `nonzeroclawed/crates/nonzeroclaw/` (213 .rs files, 143K lines)  
**0.6.7 crate:** 386 .rs files, 288K lines  
**License:** MIT OR Apache-2.0 (same as ours)  
**Attribution:** Authors field: "theonlyhennygod". Lib name: "zeroclaw", bin name: "zeroclaw".

---

## Executive Summary

0.6.7 is roughly 2x our codebase. The growth is primarily in:
- New modules (trust, verifiable_intent, sop, plugins, tui, nodes, routines, hands)
- New channels (bluesky, reddit, twitter, webhook, voice_call, voice_wake, mqtt, notion, lark, mochat)
- Expanded tools (98 files vs our 36)
- Much larger config/schema.rs (16K lines vs our 7K)

**Vendoring strategy:** Replace our nonzeroclaw crate with 0.6.7 as the base, then re-apply our two Anthropic patches and restore our vault/ module. The 0.6.7 code is a strict superset of ours for shared modules.

---

## Package Metadata (0.6.7)

```toml
name = "zeroclawlabs"
version = "0.6.7"
edition = "2024"
rust-version = "1.87"
license = "MIT OR Apache-2.0"
description = "Zero overhead. Zero compromise. 100% Rust. The fastest, smallest AI assistant."
authors = ["theonlyhennygod"]
```

**Action:** Rename to `nonzeroclaw` (our crate name), update version, add attribution note in README/LICENSE.

---

## Modules: 0.6.7 Only (New)

| Module | Files | Purpose | Priority for us |
|--------|-------|---------|----------------|
| `trust/` | 3 | Per-domain trust scoring with decay, regression detection, autonomy reduction | **HIGH** — directly addresses Agents of Chaos findings |
| `verifiable_intent/` | 6 | SD-JWT layered credential system for commerce-gated agent actions | MEDIUM — interesting for action authorization |
| `sop/` | 7 | Standard Operating Procedures engine (audit, conditions, dispatch, metrics) | MEDIUM — structured action plans |
| `commands/` | 3 | CLI subcommands (mod.rs, self_test.rs, update.rs) | LOW — CLI convenience |
| `tui/` | 4 | Terminal UI (onboarding, theme, widgets) | LOW — we don't use TUI |
| `plugins/` | ? | Plugin system | MEDIUM — extensibility |
| `nodes/` | ? | Node management | LOW — we handle this differently |
| `hands/` | 2 | Types for hand/peripheral control | LOW — hardware specific |
| `routines/` | ? | Routine execution engine | MEDIUM — could complement heartbeats |
| `i18n.rs` | 1 | Internationalization | LOW |
| `cli_input.rs` | 1 | CLI input handling | LOW |

**Recommendation:** Accept all. We don't have to use them, but having them available is valuable. Trust/ and verifiable_intent/ are immediately useful.

---

## Modules: Ours Only

| Module | Files | Purpose | Action |
|--------|-------|---------|--------|
| `vault/` | 8 | Secret management with policy, approval, Bitwarden adapter | **KEEP** — add back after vendoring |

---

## Shared Modules — Size Comparison

| Module | Ours (lines) | 0.6.7 (lines) | Ours (files) | 0.6.7 (files) | Notes |
|--------|-------------|---------------|-------------|---------------|-------|
| `providers/anthropic.rs` | 1,499 | 2,105 | — | — | **We have patches here** (MCP image + vision hardening) |
| `config/schema.rs` | 7,127 | 16,084 | — | — | 0.6.7 has 2x config surface (new channels, trust, VI, SOP) |
| `channels/mod.rs` | 6,600 | 11,627 | — | — | 0.6.7 has many new channels |
| `agent/loop_.rs` | 5,902 | 9,548 | — | — | 0.6.7 has loop_detector, context_analyzer, context_compressor |
| `gateway/mod.rs` | 3,556 | 3,687 | — | — | Similar size, likely minor additions |
| `main.rs` | 1,967 | 2,863 | — | — | 0.6.7 has CLI commands, TUI integration |
| `lib.rs` | 436 | 601 | — | — | 0.6.7 exports more modules |
| `agent/` | — | — | 8 | 18 | 0.6.7 adds: context_analyzer, context_compressor, cost, eval, history, history_pruner, personality, tests, thinking, tool_execution |
| `channels/` | — | — | 26 | 45 | 0.6.7 adds: acp_server, bluesky, debounce, discord_history, gmail_push, lark, link_enricher, media_pipeline, mochat, mqtt, notion, reddit, session_backend, session_sqlite, stall_watchdog, twitter, voice_call, voice_wake, webhook, whatsapp_storage, whatsapp_web |
| `tools/` | — | — | 36 | 98 | Massive expansion — SOP tools, VI tools, sessions, reactions, workspace, weather, linkedin, notion, polls, etc. |
| `security/` | — | — | 16 | 23 | 0.6.7 adds more security modules |
| `memory/` | — | — | 15 | 25 | 0.6.7 has more memory backends/features |
| `gateway/` | — | — | 6 | 14 | 0.6.7 has much more gateway surface |

---

## Critical Files — Detailed Comparison

### `providers/anthropic.rs` — **NEEDS MANUAL MERGE**

**0.6.7 state:** `ToolResult.content` is still `String` (plain). No MCP image support, no vision MIME validation.

**Our patches (NOT in 0.6.7):**
1. `ToolResultContent` enum (Plain | Blocks) for MCP image tool results — 114 lines added
2. Vision hardening: MIME validation, cache-control walk, `chat_with_system` delegation — 53 lines changed

**Action:** Take 0.6.7 as base (it has 600 more lines of other improvements), then re-apply both patches. The patches touch `NativeContentOut::ToolResult` and `convert_messages`/`parse_tool_result_message` — should be a clean merge since 0.6.7 hasn't changed that area.

### `config/schema.rs` — **TAKE 0.6.7**

0.6.7 is 16K lines vs our 7K. Adds config for: trust scoring, verifiable intent, SOPs, new channels (bluesky, reddit, twitter, voice), per-channel proxy, plugins. Our socks proxy fix is already included.

**Action:** Take 0.6.7 wholesale. No patches to preserve.

### `channels/mod.rs` — **TAKE 0.6.7**

0.6.7 is 11.6K lines vs our 6.6K. Adds: debounce, stall_watchdog, link_enricher, media_pipeline, many new channel types. Our formatting fix (multi-room routing) appears to be included.

**Action:** Take 0.6.7 wholesale.

### `agent/loop_.rs` — **TAKE 0.6.7**

0.6.7 is 9.5K lines vs our 5.9K. Adds: context analysis, compression, history pruning, loop detection, thinking support.

**Action:** Take 0.6.7 wholesale.

### `gateway/mod.rs` — **TAKE 0.6.7**

Similar size. Minor additions.

**Action:** Take 0.6.7 wholesale.

### `main.rs` — **TAKE 0.6.7, then customize**

0.6.7 has CLI commands, TUI, and more subcommands. Our main.rs has some NZC-specific customizations.

**Action:** Take 0.6.7, review for any NZC-specific entry points we need to preserve.

### `lib.rs` — **TAKE 0.6.7, add vault**

0.6.7 exports: trust, verifiable_intent, sop, plugins, nodes, hands, routines, tui, commands, i18n.

**Action:** Take 0.6.7, add `pub mod vault;` and our vault module.

### `Cargo.toml` — **MERGE CAREFULLY**

0.6.7 requires `rust-version = "1.87"` and `edition = "2024"`. Has ~100 deps vs our ~60.

New deps in 0.6.7 that we don't have (sampling):
- `sd-jwt-vc` (for verifiable intent)
- Various new channel deps
- `ratatui` (TUI)

Our deps not in 0.6.7:
- `outpost` (our crate — path dep)
- `clash` (our crate — path dep)
- `bitwarden-cli` (vault)
- Various wa-rs deps (WhatsApp)

**Action:** Take 0.6.7 Cargo.toml as base, add our workspace path deps (outpost, clash), vault deps, and WhatsApp deps. Rename package to `nonzeroclaw`.

---

## Vendoring Plan — Step by Step

### Phase 1: Replace (30 min)
1. Back up current `crates/nonzeroclaw/` to `crates/nonzeroclaw-backup/`
2. Copy 0.6.7 `src/` into `crates/nonzeroclaw/src/`
3. Copy 0.6.7 `build.rs`, `web/`, `tool_descriptions/` 
4. Merge Cargo.toml (0.6.7 base + our workspace deps + vault deps)
5. Rename crate: `zeroclawlabs` → `nonzeroclaw` in Cargo.toml, lib name, bin name
6. Add `pub mod vault;` to lib.rs
7. Copy our `vault/` module back into src/

### Phase 2: Patch (15 min)
8. Re-apply Anthropic MCP image blocks patch to `providers/anthropic.rs`
9. Re-apply Anthropic vision hardening patch to `providers/anthropic.rs`
10. Update any `zeroclaw::` references to `nonzeroclaw::` in our code

### Phase 3: Build & Fix (1-2 hrs)
11. `cargo check -p nonzeroclaw` — fix compilation errors
12. Likely issues: missing deps in Cargo.toml, feature flag mismatches, workspace path resolution
13. `cargo test -p nonzeroclaw` — fix test failures
14. `cargo build --release -p nonzeroclaw` — verify release build

### Phase 4: Integration (30 min)
15. Verify outpost and clash crates still compile against new nonzeroclaw
16. Update nonzeroclawed crate if it references any changed nonzeroclaw types
17. `cargo build --release` for full workspace

### Phase 5: Attribution & Docs
18. Add VENDORING.md noting: source crate, version, date, license, original authors
19. Update BUILD-NOTES.md
20. Retire `/root/projects/nonzeroclaw/` fork repo (keep as archive, mark as superseded)

---

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Compilation errors from missing deps | HIGH | LOW | Iterative cargo check/fix |
| Feature flag conflicts | MEDIUM | LOW | Align features with 0.6.7 defaults |
| Our patches don't apply cleanly | LOW | MEDIUM | Patches are localized to anthropic.rs |
| 0.6.7 has bugs we don't have | MEDIUM | LOW | We can revert specific files |
| Build time increases significantly | HIGH | LOW | Expected with 2x codebase |
| Rust 1.87 not available | LOW | HIGH | Check `rustc --version` first |

---

## Post-Vendor Opportunities

1. **Use trust/ module** for sender trust scoring (complements our sender-trust.json)
2. **Use verifiable_intent/** for cryptographic action authorization
3. **Use sop/** for structured safety procedures
4. **Use loop_detector** in agent/ to prevent resource loops (Agents of Chaos CS4)
5. **Use stall_watchdog** in channels/ for timeout management
6. **Port relevant 0.6.7 security/ improvements** into our outpost scanning
