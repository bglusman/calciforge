# VENDORING.md — Upstream Attribution

This crate is a **vendored fork** of the `zeroclawlabs` crate, adapted for the ZeroClawed ecosystem.

## Source

| Field | Value |
|-------|-------|
| **Crate** | `zeroclawlabs` v0.6.7 |
| **Registry** | [crates.io/crates/zeroclawlabs](https://crates.io/crates/zeroclawlabs) |
| **Published** | 2026-03-29 |
| **Original repo** | `github.com/zeroclaw-labs/zeroclaw` — **DELETED** as of 2026-03-30 |
| **License** | MIT OR Apache-2.0 |
| **Original authors** | theonlyhennygod |

## Vendored Into

| Field | Value |
|-------|-------|
| **Crate name** | `nonzeroclaw` (renamed from `zeroclawlabs`) |
| **Vendored version** | `0.6.7-nzc.1` |
| **Vendored date** | 2026-03-31 |
| **Vendored by** | ZeroClawed Contributors |

## License

The original source code is dual-licensed under **MIT OR Apache-2.0**.
Both `LICENSE-MIT` and `LICENSE-APACHE` files from the upstream crate are included in this directory.

ZeroClawed additions and modifications (vault module, Anthropic patches, etc.) are also
released under the same **MIT OR Apache-2.0** dual license.

## Our Modifications

### Completed at vendoring time

1. **Crate rename**: `zeroclawlabs` → `nonzeroclaw` (package name, lib name, bin name)
   - All `zeroclaw::` references in `src/` updated to `nonzeroclaw::`
   - `lib.rs` bin/lib names updated
   
2. **Vault module restored**: Our `vault/` module (8 files) was not in the upstream crate.
   It was backed up from the pre-vendoring state and restored into `src/vault/`.
   Exposed via `pub mod vault;` in `lib.rs`.

3. **Workspace integration**: 
   - Removed upstream `[workspace]` section (we're inside `zeroclawed`)
   - Added `outpost = { path = "../outpost" }` and `clash = { path = "../clash" }` path deps
   - Added `aardvark-sys` stub crate (upstream uses a path dep not published to crates.io)
   
4. **`bitwarden-cli` feature**: Retained from our pre-vendoring Cargo.toml — gates vault
   subprocess adapter code without external crate dependencies.

5. **Cargo.toml metadata**: Updated version to `0.6.7-nzc.1`, removed dead repository URL,
   added ZeroClawed attribution and vendoring comment.

### Pending — TODO (not yet applied)

> ⚠️ These patches from our pre-vendoring codebase must be re-applied manually to
> `src/providers/anthropic.rs`:

#### Patch 1: MCP Image Blocks — `ToolResultContent` enum

**Location**: `src/providers/anthropic.rs`  
**What**: Add a `ToolResultContent` enum with `Plain(String)` and `Blocks(Vec<ContentBlock>)`
variants to allow MCP tool results to return structured image/content blocks rather than
plain text strings.  
**Diff reference**: See `src-backup/providers/anthropic.rs` vs new file — approximately
114 lines added around the `NativeContentOut::ToolResult` handling and
`parse_tool_result_message`/`convert_messages` functions.

#### Patch 2: Vision Hardening — MIME Validation

**Location**: `src/providers/anthropic.rs`  
**What**: Add MIME type validation for image inputs before sending to the Anthropic API.
Validate that image media types are in the allowed set (jpeg, png, gif, webp).
Also includes cache-control walk improvements and `chat_with_system` delegation.  
**Diff reference**: See `src-backup/providers/anthropic.rs` — approximately 53 lines
changed/added in the vision/multimodal handling code.

**Reference**: These patches are documented in detail in `docs/vendor-067-comparison.md`.

## Upstream New Modules (available for use)

The following modules are new in 0.6.7 and not present in our pre-vendoring codebase.
They are included in the vendored source but not yet wired into ZeroClawed configuration:

| Module | Purpose |
|--------|---------|
| `trust/` | Per-domain trust scoring with decay, regression detection, autonomy reduction |
| `verifiable_intent/` | SD-JWT layered credential system for commerce-gated agent actions |
| `sop/` | Standard Operating Procedures engine |
| `tui/` | Terminal UI (onboarding, theme, widgets) |
| `plugins/` | WASM plugin system (extism-based) |
| `nodes/` | Node management |
| `hands/` | Types for hand/peripheral control |
| `routines/` | Routine execution engine |
| `commands/` | CLI subcommands (self_test, update) |
| `i18n.rs` | Internationalization support |

See `docs/vendor-067-comparison.md` for detailed notes on each module's relevance.
