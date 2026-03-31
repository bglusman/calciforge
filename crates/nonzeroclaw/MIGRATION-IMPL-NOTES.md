# Migration Implementation Notes — Session 2

_Session: 2 of 4 (Channel Assignment + OpenClaw Migration)_
_Date: 2026-03-30_

---

## What Was Implemented

### New module: `crates/nonzeroclaw/src/onboard/migration.rs`

Core data structures and logic for OpenClaw → NZC migration:

#### Detection (`detect_openclaw_installation`, `detect_from_dir`)
- Looks for `~/.openclaw/openclaw.json` or `openclaw.jsonc`
- Parses config using a comment-stripping JSON5 parser (`parse_json5_relaxed`, `strip_json_comments`)
- Detects workspace path (config-specified or `~/.openclaw/workspace/`)
- Detects `MEMORY.md` and `memory/` daily notes directory
- Returns `Ok(None)` if no OpenClaw installation found — NZC installation is never gated on this

#### Data structures (all public)
- `OpenClawInstallation` — everything we know about an existing install
- `DetectedChannel` — a channel found in OpenClaw config with enabled/credential status
- `ChannelOwner` (enum: `Nzc` / `OpenClaw` / `Unassigned`) + `ChannelAssignment`
- `ConfigMigrationPlan` + `MappedField` + `UnmappedField`
- `MemoryMigrationOutcome` (enum: `Skipped` / `Completed` / `Failed` / `NoMemoryFound`)
- `MemoryMigrationOptions` — dest workspace path + max content bytes

#### Channel detection (`detect_channels`)
- Handles both `channels.<name>` (newer OpenClaw) and `plugins.entries.<name>` (older OpenClaw)
- Detects: telegram, signal, whatsapp, matrix, discord
- Reports enabled status and credential presence per-channel

#### Config field mapping (`build_config_migration_plan`, `plan_present_fields`)
- Known field map in `FIELD_MAP` constant (13 mappings: agent/model, gateway, API keys)
- Unmapped fields list in `UNMAPPED_FIELDS` (plugins, skills, compaction, hooks — no NZC equivalent)
- Only reports unmapped fields that are actually present in the config
- Never drops unknown fields silently — they go in the `unmapped` list

#### Memory migration (`migrate_memory`, `run_migrate_memory_command`)
- **Not a file copy** — uses LLM to clean and reframe content for NZC context
- Preserves: user/infra facts, historical decisions, communication preferences
- Removes: OpenClaw-specific config details, OpenClaw tattoos, OpenClaw commands
- Truncates input to 64 KiB by default to stay within context limits
- `AsyncLlmFn` trait decouples migration from NZC's provider infrastructure
- `StubLlmFn` — returns placeholder; used until session 3/4 wires up a real provider
- `FailingLlmFn` — for test error-path coverage
- `run_migrate_memory_command` — entry point for `nzc migrate-memory` subcommand (stub)

### Wizard integration: `crates/nonzeroclaw/src/onboard/wizard.rs`

Added step 2 of 10 (was 9 steps, now 10) to `run_wizard`:

- **`setup_openclaw_migration`** — detects OpenClaw, offers migration, orchestrates sub-steps
  - Returns `MigrationWizardResult` (install, channel assignments, config confirm, memory outcome)
  - Returns default/empty result if no OpenClaw found (wizard continues normally)
  
- **`run_channel_assignment_step`** — interactive Select for each detected channel
  - Options: "NZC (this install)" / "OpenClaw (leave unchanged)" / "Skip"
  - Shows enabled/disabled status and credential presence before asking
  
- **`run_config_migration_step`** — shows field diff, redacts credentials, requires confirmation
  - Displays mapped fields (OpenClaw path → NZC path = value)
  - Lists unmapped fields with reason
  - Shows `🔒` notice that OpenClaw config is read-only
  
- **`run_memory_migration_step`** — offers LLM-assisted memory migration
  - Tells user it uses LLM, not a plain copy
  - Offers "do this later with `nzc migrate-memory`" escape hatch
  - Currently uses `StubLlmFn` (writes placeholder MEMORY.md)
  
- **`setup_channels_with_migration`** — applies NZC-assigned channels after channel step
  - Pulls Telegram, Discord, Signal credentials from OpenClaw snippet
  - Fills all struct fields explicitly (no `..Default::default()` — these types don't impl Default)
  - Matrix: notes manual config required (complex auth)
  - WhatsApp: notes re-link required (Baileys session not portable)

- **`apply_channel_from_openclaw`** — handles per-channel credential extraction
  - Tries multiple field name variants (camelCase + snake_case) for each channel

---

## What's Stubbed

### `run_migrate_memory_command` (real LLM call)
```rust
// TODO (session 3/4): replace StubLlmFn with NZC's real provider client
Ok(migrate_memory(&install, &opts, StubLlmFn).await)
```
The interface is production-ready. Session 3/4 needs to:
1. Accept a `Box<dyn AsyncLlmFn>` from the CLI caller
2. Construct it from `Config::load()` + NZC's provider factory
3. Pass it into `run_migrate_memory_command`

The `AsyncLlmFn` trait signature:
```rust
pub trait AsyncLlmFn: Send + Sync {
    fn call<'a>(&'a self, prompt: &'a str) 
        -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;
}
```

### `nzc migrate-memory` subcommand registration
`run_migrate_memory_command` exists and is tested but is not yet wired into `main.rs` as a CLI subcommand. Session 3/4 should add it to the `clap` command tree.

### OpenClaw config write-back (intentionally NOT implemented)
After channel assignment, the plan calls for disabling migrated channels in OpenClaw's config. This is **deliberately deferred** — it requires:
- The Workstream 2 adapter flow (gateway health check, backup, dry-run diff, apply)
- Version compatibility check
- Rollback path

Session 3 (PolyClaw adapter installation) owns this. The `ChannelAssignment` data from session 2 is the handoff artifact.

---

## What Session 3 Needs From This Code

### The `MigrationWizardResult` struct
Returned from `setup_openclaw_migration`. Session 3 needs:
```rust
pub struct MigrationWizardResult {
    pub install: Option<OpenClawInstallation>,
    pub channel_assignments: Vec<ChannelAssignment>,
    pub config_migration_confirmed: bool,
    pub memory_outcome: Option<MemoryMigrationOutcome>,
}
```

Specifically:
- `channel_assignments` where `owner == ChannelOwner::OpenClaw` → these channels should be **disabled** in OpenClaw's config by the PolyClaw adapter
- `install.config_path` → path to OpenClaw config for the adapter to modify (with backup first)
- `config_migration_confirmed` → if true, apply the mapped fields to NZC config

### The `AsyncLlmFn` trait
For wiring up the real LLM call in `run_migrate_memory_command`.

### Public API surface (all in `crates/nonzeroclaw::onboard::migration`)
All types and functions needed for session 3/4 are re-exported from `crates/nonzeroclaw::onboard`.

---

## Test Coverage

27 tests in `onboard::migration::tests`:

| Category | Tests |
|----------|-------|
| Detection (unit) | `no_openclaw_dir_returns_none`, `openclaw_dir_without_config_returns_none`, `malformed_config_returns_error`, `minimal_valid_config_parsed`, `config_with_version_parsed` |
| Channel detection (unit) | `detect_telegram_channel_with_token`, `detect_channel_missing_credentials`, `detect_channel_disabled`, `detect_channel_via_plugins_entries`, `config_with_no_channels_returns_empty_list` |
| Config mapping (unit) | `config_migration_plan_maps_present_fields`, `config_migration_plan_missing_fields_have_none_value`, `config_migration_plan_flags_unmapped_plugins` |
| JSON parsing (unit) | `strip_json_comments_line_comment`, `strip_json_comments_block_comment`, `strip_json_comments_preserves_url_in_string` |
| Memory (unit + async) | `memory_files_collected_from_workspace`, `migrate_memory_no_files_returns_no_memory_found`, `migrate_memory_stub_llm_writes_file`, `migrate_memory_failing_llm_returns_failed`, `run_migrate_memory_command_no_openclaw_returns_no_memory` |
| Summary / display | `installation_summary_format`, `channel_owner_display` |
| Property tests (hegel) | `detect_channels_never_panics`, `config_migration_plan_never_panics`, `mapped_fields_always_have_known_nzc_paths`, `strip_comments_idempotent_on_clean_json` |

**Note:** Hegel property tests require `uv` on PATH. Install with: `curl -LsSf https://astral.sh/uv/install.sh | sh`
This is a pre-existing requirement for all hegel tests in this crate (see `tests/hegel_smoke.rs`).

---

## Files Changed

| File | Change |
|------|--------|
| `src/onboard/migration.rs` | **New** — complete migration module |
| `src/onboard/mod.rs` | Added `pub mod migration` + re-exports |
| `src/onboard/wizard.rs` | Added step 2/10, new helper functions, migration import |

## Files NOT Changed (intentional)
- `~/.openclaw/openclaw.json` — OpenClaw config is read-only, never written
- Any OpenClaw workspace files — read-only
- `Cargo.toml` — no new dependencies needed (json5 parsing done manually; tokio/serde/anyhow already present)
