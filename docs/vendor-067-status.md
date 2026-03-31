# Vendor 0.6.7 Status Report

**Date:** 2026-03-31  
**Executed by:** Subagent (polyclaw-mono vendoring task)  
**Status:** ✅ Phase 1–5 COMPLETE — `cargo check -p nonzeroclaw` passes with zero errors

---

## What Was Done

### Phase 1: Backup and Replace ✅

1. Backed up `crates/nonzeroclaw/src/` → `crates/nonzeroclaw/src-backup/` (35 items preserved)
2. Replaced `src/` with 0.6.7's `src/` (386 .rs files, 288K lines)
3. Restored our `vault/` module from backup: 8 files (`adapter.rs`, `approval.rs`, `bitwarden.rs`, `config.rs`, `error.rs`, `manager.rs`, `mod.rs`, `types.rs`)
4. Copied 0.6.7's `build.rs`
5. Copied 0.6.7's `web/` directory (full TypeScript frontend)
6. Copied 0.6.7's `tool_descriptions/` (31 locale TOML files)

### Phase 2: Cargo.toml Merge ✅

- Started from 0.6.7's `Cargo.toml.orig` as base
- Renamed: `name = "zeroclawlabs"` → `name = "nonzeroclaw"`
- Renamed bin: `name = "zeroclaw"` → `name = "nonzeroclaw"`
- Renamed lib: `name = "zeroclaw"` → `name = "nonzeroclaw"`
- Removed `[workspace]` section (we're inside polyclaw-mono workspace)
- Added `outpost = { path = "../outpost" }` and `clash = { path = "../clash" }` path deps
- Kept `bitwarden-cli` feature from our old Cargo.toml
- Removed repository URL (repo is dead), added vendoring comment header
- Updated version to `"0.6.7-nzc.1"`
- Removed dead upstream `[workspace]` sub-crates reference
- Fixed `[[test]]` entries for non-existent upstream test files (commented out)
- Fixed `aardvark-sys` path dep: `crates/aardvark-sys` → `../aardvark-sys`
- Added `aardvark-sys` stub crate at `crates/aardvark-sys/` (see below)

**New dependency resolved**: `aardvark-sys` is a path dep in upstream that wasn't published
to crates.io. Created a stub implementation at `crates/aardvark-sys/` with the full
`AardvarkHandle` API surface (all methods return `Err("SDK not available")`).
Added `crates/aardvark-sys` to the workspace `members` list.

### Phase 3: lib.rs Update ✅

Added `pub mod vault;` to `src/lib.rs` (between `tunnel` and `verifiable_intent`).

### Phase 4: Fix References ✅

- Searched all `.rs` files for `zeroclaw::` references
- Fixed 14 live code references in `src/main.rs` (uses `nonzeroclaw::GatewayCommands`, etc.)
- Fixed doc comment references in `providers/telnyx.rs`, `util.rs`, `tools/schema.rs`
- String literal occurrences (protocol client IDs like `"zeroclaw"` in discord/mqtt/irc)
  were intentionally left as-is — these are wire protocol values, not Rust types

### Phase 5: Compilation ✅

```
Finished `dev` profile [unoptimized + debuginfo] target(s) in ~1m 02s
```

**Zero errors. Zero warnings.** `cargo check -p nonzeroclaw` and `cargo check --bin nonzeroclaw` both pass cleanly.

### Phase 6: Patches (NOT applied — noted for future work) ✅

Patches documented in `VENDORING.md`. Not attempted as instructed.

### Phase 7: Attribution ✅

- Created `crates/nonzeroclaw/VENDORING.md` with full attribution
- Copied `LICENSE-APACHE` and `LICENSE-MIT` from 0.6.7
- `src-backup/` directory preserved (NOT deleted)

---

## What Remains

### HIGH PRIORITY — Must do before production use

1. **Re-apply Anthropic MCP image blocks patch** (`src/providers/anthropic.rs`)
   - Add `ToolResultContent` enum (Plain | Blocks)
   - Approx 114 lines, touches `NativeContentOut::ToolResult`, `parse_tool_result_message`, `convert_messages`
   - Reference: `src-backup/providers/anthropic.rs` vs new file

2. **Re-apply vision hardening patch** (`src/providers/anthropic.rs`)
   - MIME type validation for image inputs
   - Approx 53 lines, touches vision/multimodal handling
   - Reference: `src-backup/providers/anthropic.rs`

### MEDIUM PRIORITY — Configuration and wiring

3. **Wire outpost/clash integration** — The `outpost` and `clash` crates are now path deps
   but nonzeroclaw doesn't yet call into them. Review `src-backup/` for how we previously
   used these crates and restore those call sites.

4. **Explore new 0.6.7 modules** for PolyClaw use:
   - `trust/` — sender trust scoring (directly useful for Agents of Chaos defenses)
   - `verifiable_intent/` — cryptographic action authorization
   - `sop/` — structured safety procedures
   - `loop_detector` in `agent/` — prevent resource loops

5. **Run `cargo test -p nonzeroclaw`** to see which integration tests pass/fail

6. **Run full workspace build** `cargo check` to verify outpost/clash/polyclaw still OK

### LOW PRIORITY

7. **Update CLAUDE.md** in `crates/nonzeroclaw/` to reflect 0.6.7 vendoring
8. **Consider retiring `/root/projects/nonzeroclaw/`** fork (if it exists) — mark superseded

---

## Files Changed

| Path | Action |
|------|--------|
| `crates/nonzeroclaw/src/` | Replaced with 0.6.7 (386 files) |
| `crates/nonzeroclaw/src-backup/` | Created (backup of pre-vendoring state) |
| `crates/nonzeroclaw/src/vault/` | Restored from backup |
| `crates/nonzeroclaw/src/lib.rs` | Added `pub mod vault;` |
| `crates/nonzeroclaw/src/main.rs` | Fixed `zeroclaw::` → `nonzeroclaw::` (14 refs) |
| `crates/nonzeroclaw/src/**/*.rs` | Fixed doc comment `zeroclaw::` refs |
| `crates/nonzeroclaw/Cargo.toml` | Full merge (0.6.7 base + our deps, renames) |
| `crates/nonzeroclaw/build.rs` | Replaced with 0.6.7 version |
| `crates/nonzeroclaw/web/` | Replaced with 0.6.7 TypeScript frontend |
| `crates/nonzeroclaw/tool_descriptions/` | Replaced with 0.6.7 (31 locales) |
| `crates/nonzeroclaw/LICENSE-APACHE` | Updated from 0.6.7 |
| `crates/nonzeroclaw/LICENSE-MIT` | Updated from 0.6.7 |
| `crates/nonzeroclaw/VENDORING.md` | Created (new) |
| `crates/aardvark-sys/` | Created stub crate (new) |
| `Cargo.toml` (workspace root) | Added `crates/aardvark-sys` to members |
