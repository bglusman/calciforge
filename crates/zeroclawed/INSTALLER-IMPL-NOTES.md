# ZeroClawed Multi-Target Installer — Implementation Notes

_Session 4 of the zeroclawed sprint. Written for the Opus review session._

---

## What Was Implemented

### Module structure: `crates/zeroclawed/src/install/`

| File | Contents |
|------|----------|
| `mod.rs` | Entry point: `run(args)` → interactive wizard or non-interactive pipeline |
| `model.rs` | Core data model: `ClawAdapter`, `ClawTarget`, `InstallTarget`, `VersionCompatibility`, `backup_filename` |
| `cli.rs` | CLI arg parsing: `InstallArgs`, `parse_claw_spec`, `parse_install_target` |
| `ssh.rs` | SSH abstraction: `SshClient` trait, `RealSshClient`, `MockSshClient`, `shell_quote` |
| `health.rs` | Health check abstraction: `HealthChecker` trait, `HttpHealthChecker`, `MockHealthChecker` |
| `executor.rs` | 9-step install pipeline with rollback, dry-run, injectable deps |
| `wizard.rs` | Interactive TUI wizard using `dialoguer` (8-step flow) |

### Key design decisions made

**`ClawAdapter` enum (corrected from original spec)**  
The key axis is *remote configurability*, not adapter protocol:
- `NzcNative` and `OpenClawHttp` → SSH-configurable, installer knows config format
- `OpenAiCompat`, `Webhook`, `Cli` → endpoint-only, installer just registers them

`ClawTarget.ssh_key` is `Option<PathBuf>` — only required for SSH-configurable adapters.

**CLI flag format**
```
--claw name=foo,adapter=nzc,host=user@host,key=/path,endpoint=http://...
--claw name=bar,adapter=openclaw,host=user@host,key=/path,endpoint=http://...
--claw name=baz,adapter=openai-compat,endpoint=http://claw/v1
--claw name=qux,adapter=webhook,endpoint=http://hook/receive,format=json
--claw name=bin,adapter=cli,command=/usr/local/bin/my-claw
```

**MockHealthChecker sequential responses**  
Added `push_ok()` / `push_err()` for sequential call ordering, in addition to static `set_healthy()` / `set_unhealthy()`. This is needed because baseline and post-apply health checks hit the same endpoint URL but need different responses.

**shell_quote correctness**  
Uses POSIX `'\''` idiom. The test initially checked "no single-quotes in inner content" which was wrong — `'\''` is the correct escape and does produce `'` in the raw string. Tests now verify the correct invariant (presence of escape idiom for inputs with `'`).

---

## What's Stubbed

### `apply_remote_config` (executor.rs: `apply_zeroclawed_marker`)

**This is the most important stub.** Currently writes a `_zeroclawed_registered: true` marker to the remote config. Production implementation should:

**For `OpenClawHttp`:**
- Parse `openclaw.json` fully (use the JSON5-relaxed parser from `nonzeroclaw::migration`)
- Add `hooks.enabled = true` if not present
- Add a `hooks.token` with a generated shared secret
- Possibly add a named `zeroclawed` plugin entry (depends on what OpenClaw schema version supports)
- Write back with `write_file` — never overwrite without backup
- The exact field names depend on the target's version (use `meta.lastTouchedVersion`)

**For `NzcNative`:**
- Parse `~/.config/nzc/config.toml`
- Add a `[zeroclawed]` upstream section with the ZeroClawed endpoint + token
- The NZC config schema is in `nonzeroclaw::config`

The stub is tested (marker appears in config, idempotent on second run) but the real JSON/TOML patching is left for the Opus session.

### Version detection fallback

`detect_openclaw_version` tries `jq` first, then grep. It does NOT try the OpenClaw HTTP API (`GET /version`) because that requires an auth token we may not have yet. Real implementation could add a `GET <endpoint>/version` HTTP probe as a third fallback.

### Wizard channel routing

Step 4 (channel routing) collects `ChannelRouting` assignments but doesn't actually write them anywhere. The `ChannelRouting` struct is the right shape to feed into `PolyConfig::routing` entries. The Opus session should:
1. Generate `[[routing]]` TOML entries from the wizard's collected assignments
2. Write them to `~/.zeroclawed/config.toml` (or the specified config path)
3. Optionally disable the same channels in the OpenClaw config (with backup)

### Vault integration

No vault integration yet. Generated shared secrets (e.g. `hooks.token`) are currently not generated at all (the stub just registers). When vault support lands (Workstream 1), `apply_remote_config` should:
1. Generate a shared secret
2. Store it in vault via `VaultAdapter::store_secret`
3. Write only the reference (not the plaintext) to configs

---

## Testing Coverage

**310 tests total (all passing).** New installer tests:

| Module | Tests | Coverage |
|--------|-------|----------|
| `model` | 15 (incl. 3 hegel property tests) | `ClawAdapter`, version compat, `backup_filename` |
| `cli` | 22 | spec parsing, error cases, injection safety |
| `ssh` | 20 | `shell_quote`, mock SSH, connectivity, version detection |
| `health` | 10 | mock health checker, skip for CLI, sequential responses |
| `executor` | 13 | full pipeline, rollback, dry-run, non-SSH adapters |
| `wizard` | 5 | TTY guard, display helpers, struct fields |

**Hegel property tests:**
- `prop_backup_unique_timestamps` — distinct timestamps → distinct backup names
- `prop_backup_contains_timestamp` — backup always contains the timestamp
- `prop_shell_quote_safe` — shell_quote always produces safe POSIX-quoted output

**Notable test patterns:**
- `post_apply_health_check_failure_triggers_rollback` — uses sequential health responses to simulate healthy baseline then failed post-apply
- `dry_run_makes_no_ssh_writes` — verifies no write SSH calls are made in dry-run mode
- `shell_quote_arbitrary_paths_no_injection` — covers multiple injection vectors

---

## Shared Structs Between NZC and ZeroClawed

The Opus review session should consider extracting these into a shared crate:

### Currently in `nonzeroclaw::onboard::migration` only:

- `OpenClawInstallation` — needed by ZeroClawed installer to read OpenClaw config
- `DetectedChannel` / `ChannelOwner` / `ChannelAssignment` — needed for installer's channel routing step
- `parse_json5_relaxed` / `strip_json_comments` — needed by `apply_remote_config` to parse openclaw.json
- `detect_openclaw_installation` / `detect_channels` — useful in the installer flow

### Proposed shared crate: `crates/zeroclawed-shared` or `crates/claw-types`

Moving these to a shared crate avoids the circular dependency (zeroclawed can't depend on nonzeroclaw). Both crates would then depend on `claw-types`.

The ZeroClawed installer currently has its own minimal `strip_json_comments_simple` (in executor.rs) — this is a stopgap. The real one from migration.rs is better (handles edge cases, tested).

---

## Safety Protocol Compliance

From Workstream 2 requirements:

| Requirement | Status |
|-------------|--------|
| Backup first | ✅ Backup taken and verified before any write |
| Verify backup | ✅ `verify_file_exists` after `backup_file` |
| Version check | ✅ `check_version_compatibility` with Unknown/Incompatible verdicts |
| Dry run | ✅ `--dry-run` shows all planned operations, no writes |
| Rollback path | ✅ Automatic rollback on post-apply health check failure |
| One claw at a time | ✅ Sequential per-claw loop; no parallel mutations |
| Health check after | ✅ Post-apply health check; rollback if it fails |
| Config never written without backup | ✅ `run_apply` checks `backup_path.is_none()` |
| `--skip-backup` requires explicit flag | ✅ Warned as DANGEROUS in output |

**Known gap:** The `--yes` flag (skip confirmations) is parsed but the executor doesn't use it yet. The non-interactive flow always proceeds without prompts (which is the right behavior); the interactive wizard uses `Confirm::new()`. The `--yes` flag should be plumbed through to suppress the wizard's `Confirm` prompts for fully scripted use.

---

## What the Opus Review Session Should Focus On

1. **`apply_remote_config` real implementation** — the most important stub. Needs actual JSON/TOML patching for both OpenClaw and NZC formats. Should use the migration module's JSON parser.

2. **Shared crate extraction** — `OpenClawInstallation`, `DetectedChannel`, `parse_json5_relaxed` into a crate both NZC and ZeroClawed can use.

3. **`--yes` flag propagation** — wire through to suppress `Confirm` dialogs in scripted runs.

4. **Config write-back** — the wizard collects channel routing assignments but doesn't persist them to `~/.zeroclawed/config.toml`. This needs implementing.

5. **Vault integration hooks** — `apply_remote_config` should call `VaultAdapter::store_secret` for generated credentials when vault is configured.

6. **SSH key validation** — currently we accept any path; should verify the key file exists and has correct permissions (600) before attempting SSH.

7. **`openclaw.json` write-back via API vs. file** — the planning doc notes that POST `/config` may be the right path for newer OpenClaw versions. The SSH file-edit approach works but may bypass schema validation the gateway does on startup. Worth investigating.

8. **Error messages for rollback failure** — when rollback fails (`RollbackStatus::Failed`), the installer prints a warning but doesn't know how to guide the operator. Should print the exact rollback command they can run manually.
