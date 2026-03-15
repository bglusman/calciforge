# BUILD-NOTES.md

## Monorepo Migration — 2026-03-14

### What was done

Consolidated `/root/projects/polyclaw-v2/` and `/root/projects/nonzeroclaw/` into a single
Cargo workspace at `/root/projects/polyclaw-mono/`.

### Crate layout

```
crates/
  polyclaw/     ← copied from polyclaw-v2/polyclaw/ (unchanged)
  nonzeroclaw/  ← src/ + web/ + tests/ etc from nonzeroclaw/ (new Cargo.toml, no nested workspace)
  outpost/      ← copied from polyclaw-v2/outpost/ (canonical; identical to nonzeroclaw/crates/outpost/)
  clash/        ← copied from nonzeroclaw/crates/clash/
  robot-kit/    ← copied from nonzeroclaw/crates/robot-kit/
```

### Outpost — both copies were identical

`diff -rq polyclaw-v2/outpost/src/ nonzeroclaw/crates/outpost/src/` → IDENTICAL

Used `polyclaw-v2/outpost/` as the canonical copy. Both polyclaw and nonzeroclaw now reference
`../outpost` (relative path within the workspace).

### nonzeroclaw — nested workspace removed

Original `nonzeroclaw/Cargo.toml` had `[workspace]` with members `[".", "crates/robot-kit", "crates/outpost", "crates/clash"]`.
New `crates/nonzeroclaw/Cargo.toml` has only `[package]` — workspace membership is handled by the root.

### web/dist fix

`static_files.rs` embeds `web/dist/` at compile time using `rust-embed`. Initial copy omitted the
`web/` directory, causing a compile error. Fixed by copying `nonzeroclaw/web/` → `crates/nonzeroclaw/web/`.

### Build result

```
cargo check   → 0 errors, ~30 pre-existing warnings (all inherited from originals)
cargo build --release -p polyclaw -p nonzeroclaw → SUCCESS (2m47s)
  target/release/polyclaw     5.6MB
  target/release/nonzeroclaw  16MB
```

### Deploy result (2026-03-14)

Target: root@10.0.0.10 (nullclaw-test, PVE kernel)

```
polyclaw     → /usr/local/bin/polyclaw   ✓ running
nonzeroclaw  → /usr/local/bin/nonzeroclaw ✓ running
```

Post-deploy journal (last 5 lines each):
- polyclaw: "PolyClaw v2 starting … Telegram bot connected username=example_bot"
- nonzeroclaw: "GET /ws/chat — WebSocket agent chat … Press Ctrl+C to stop."

### Disk

Before build: 53G used / 2.0T
After release build: 56G used
After `rm -rf target/debug`: 55G used (~935MB release target)

### Original repos (preserved)

- `/root/projects/polyclaw-v2/` — NOT deleted (kept as reference)
- `/root/projects/nonzeroclaw/` — NOT deleted (kept as reference)

Safe to delete originals once you're satisfied with the monorepo.

### Pre-existing warnings (not introduced by migration)

- polyclaw: unused imports, dead_code, deprecated `msg.from()` (teloxide API change)
- nonzeroclaw: unused imports in security/, tools/, channels/

These existed in the original repos before migration.

### TODO (future)

- [ ] Create `crates/common/` for shared types if/when polyclaw and nonzeroclaw develop type overlap
- [ ] Migrate shared deps to `workspace = true` in all crate Cargo.tomls
- [ ] Fix pre-existing compiler warnings
- [ ] Add CI (GitHub Actions or similar)
- [ ] Consider deleting originals after a burn-in period
