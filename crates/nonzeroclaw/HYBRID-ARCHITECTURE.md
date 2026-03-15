# NonZeroClaw — Hybrid Architecture

## Strategy

NonZeroClaw uses a hybrid fork model:

- **Vendored modules** (in-tree source): gateway, agent loop, OpenAI compat, providers/anthropic — these contain our approval flow, clash policy integration, per-sender history, and outpost scanning. We own and maintain these.
- **Upstream dependency** (`zeroclawlabs = "0.4"`, optional feature): stable infrastructure we don't modify — tools, memory, observability, runtime, most channels. Once wired, upstream fixes arrive via `cargo update` without manual backport.

## Why

- Minimizes backport work: upstream fixes to vendored modules still need manual review, but tool/memory/observability fixes arrive via `cargo update`.
- Keeps approval flow and clash policy under our control without upstreaming internal details.
- Allows us to propose extension hooks upstream over time (policy callback, tool-exec hook) that would eventually let us remove vendored modules entirely.

## Upstream Sync Process

```bash
# Check what upstream has changed in our vendored files
bash scripts/upstream-sync.sh

# To extract a patch for a specific upstream fix:
cd /root/projects/nonzeroclaw
git format-patch aa45c30..v0.3.2 -- src/providers/anthropic.rs
# Then review and apply to crates/nonzeroclaw/src/providers/anthropic.rs
```

## Vendored Modules (manual backport required)

| Module | Why vendored | Key changes |
|--------|-------------|-------------|
| `src/gateway/mod.rs` | Approval flow, routes, AppState | `pending_approvals`, `pending_results`, `webhook_histories`, `policy` fields; `ReviewPendingError` handler; anonymous webhook fixed to use `run_gateway_webhook_anonymous` (routes through clash policy with `__anonymous__` sentinel key, not `run_gateway_chat_simple`) |
| `src/agent/loop_.rs` | Clash policy integration | `ReviewPendingError`, per-sender history, `process_message_with_history_and_policy` |
| `src/gateway/openai_compat.rs` | OpenAI-compatible endpoint + outpost scanning | Outpost injection scanning before forwarding |
| `src/providers/anthropic.rs` | Consecutive same-role message merge, empty content filter | `consecutive same-role merging`, skip empty/whitespace assistant text blocks (165 changed lines vs upstream) |
| `src/heartbeat/engine.rs` | Two-phase heartbeat helpers (partially wired) | `TaskPriority`, `HeartbeatTask`, structured task types (416 changed lines vs upstream) |
| `src/channels/mod.rs` | show_tool_calls scaffold | `show_tool_calls` field stub (36 changed lines vs upstream) |
| `src/config/schema.rs` | Our config additions | `show_tool_calls`, `ChannelsConfig` additions (13 changed lines vs upstream) |
| `src/providers/mod.rs` | NONZEROCLAW_PROVIDER_URL env var override | Runtime URL override without config.toml changes (39 changed lines vs upstream) |

## Module Diff Summary (as of fork from aa45c30, upstream at v0.3.2)

Upstream has **33+ commits** touching `src/agent/loop_.rs` and **23+ commits** touching `src/gateway/mod.rs` between our fork point and v0.3.2. Notable upstream additions we haven't yet backported:

- MCP subsystem tools (`tool_search`, multi-transport client) — agent loop
- Embedding routes in gateway + agent loop
- Dynamic node discovery — gateway
- Cron run history API — gateway
- Interactive session state persistence — agent loop
- HTTP request timeout configurable — gateway

These should be reviewed and backported selectively.

## Upstream Dependency (`zeroclawlabs = "0.4"`)

Added to `Cargo.toml` as **optional** (`default-features = false`). Builds cleanly alongside our codebase with no type conflicts.

**Feature gate added (2026-03-15):**
```toml
# In Cargo.toml [features] section:
zeroclawlabs = ["dep:zeroclawlabs"]
```

Activate with `--features zeroclawlabs` or `features = ["zeroclawlabs"]` in workspace Cargo.toml.

**Build validation (2026-03-15):** `cargo build -p nonzeroclaw --features zeroclawlabs` → ✅ zero errors.
No type or name conflicts between our in-tree modules and zeroclawlabs 0.4.

### Passthrough Module Status

| Module | Status | Notes |
|--------|--------|-------|
| `src/observability/` | **Near-passthrough** (validated, not yet re-exported) | One delta: zeroclawlabs `ToolCallStart` has extra `arguments: Option<String>` field. Our codebase doesn't set it; additive change. Re-export ready once we update our `ToolCallStart` call sites to pass `arguments: None`. |
| `src/memory/` | Planned | Zero diff vs upstream ref |
| `src/runtime/` | Planned | Zero diff vs upstream ref |
| `src/tools/` | Planned | Most tools unmodified |
| `src/channels/*` (except mod.rs) | Planned | Per-channel implementations unmodified |
| `src/providers/*` (except anthropic.rs, mod.rs) | Planned | Most providers unmodified |

**Next wiring step:** Update `ToolCallStart` call sites to pass `arguments: None`, then replace
`src/observability/mod.rs` with:
```rust
#[cfg(feature = "zeroclawlabs")]
pub use zeroclawlabs::observability::*;
// (keep in-tree impl as #[cfg(not(feature = "zeroclawlabs"))] fallback)
```

**Planned passthrough modules** (next batch after observability):
- `src/tools/` — most tools (web_search, file ops, etc.) are unmodified
- `src/memory/` — no NZC-specific changes
- `src/runtime/` — no NZC-specific changes
- Most channel implementations (all except channels/mod.rs header)
- Most providers (all except anthropic.rs and providers/mod.rs)

## REQUEST_TIMEOUT_SECS

NZC extends the upstream 30s timeout to **180s** to accommodate long-running approval flows and tool calls. The test `security_timeout_is_30_seconds` has been updated to assert 180 to reflect this intentional change.

## Known Pre-existing Test Failures

The following tests were failing before NZC work began and are excluded from success criteria:
- `security::prompt_guard::tests::blocking_mode_works`
- `security::prompt_guard::tests::detects_secret_extraction`

Both are in `src/security/prompt_guard.rs` and appear to be pattern-matching sensitivity issues unrelated to our changes.

## Current Test Status

- **2908 tests passing** (as of 2026-03-15, after anonymous webhook fix)
- **2 failing** (known pre-existing, see above)
- **79 clash crate tests passing**
- **1 clash doc test passing**

### New tests added (2026-03-15 hybrid wiring session)
- `gateway::tests::anonymous_webhook_uses_sentinel_key_not_simple_path` — structural guard: `__anonymous__` sentinel is stable and non-colliding
- `gateway::tests::anonymous_webhook_routes_through_policy` — proves MockProvider is never called directly by anonymous webhooks; request goes through the agent loop (policy-enforced)
