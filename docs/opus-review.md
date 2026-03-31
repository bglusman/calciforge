# Opus Code Review — 2026-03-30

_Reviewer: Claude Opus 4.6 (subagent session)_
_Scope: Sessions 1–4 of polyclaw-mono vault/migration/installer sprint_
_Lines reviewed: ~6,873 new across vault, migration, and installer modules_

---

## Critical Issues (must fix before merge)

### C1. `Secret` implements `Clone` — defeats zeroing intent

**File:** `crates/nonzeroclaw/src/vault/types.rs:12`

```rust
#[derive(Clone)]
pub struct Secret {
    value: String,
}
```

`Secret` has a carefully written `Drop` impl that zeros the buffer, then derives `Clone`. Every `.clone()` creates a copy of the secret value that _won't_ be tracked or zeroed when the original is dropped. The compiler may also elide the drop for optimized-out clones.

**Fix:** Remove `Clone` from `Secret`. Callers that need the value should call `.expose()` and handle the `&str` directly. If cloning is genuinely needed, make it explicit via a `fn duplicate(&self) -> Self` method with a doc comment explaining the security tradeoff.

Same issue applies to `SecretValue` (line 56) and `SessionToken` (line 74) — both derive `Clone`.

### C2. `ProcessBwRunner::unlock` passes master password as CLI argument

**File:** `crates/nonzeroclaw/src/vault/bitwarden.rs:62`

```rust
cmd.args(["unlock", "--raw", master_password])
```

The master password appears in the process argument list, which is visible via `/proc/<pid>/cmdline` on Linux and `ps aux` on any Unix. Any user with read access to `/proc` can extract it during the (short) window the process runs.

**Fix:** Use `bw unlock --raw` with the password supplied via stdin, or via the `BW_MASTER_PASSWORD` environment variable passed to the child process (not the parent's env). The subprocess env approach is better because the child's env is protected by permissions, unlike its argv.

### C3. `TimeBound` cache logic uses wrong expiry calculation

**File:** `crates/nonzeroclaw/src/vault/manager.rs:105-110`

```rust
SecretPolicy::TimeBound { ttl } => {
    let expires_at = Instant::now() + *ttl;
    if self.is_approved_cached(key, Some(expires_at)).await {
```

Every call to `access_secret` with a `TimeBound` policy computes a _new_ `expires_at` from `Instant::now()`, then passes it to `is_approved_cached` — but that function ignores the parameter entirely:

```rust
async fn is_approved_cached(&self, key: &str, _expected_expiry: Option<Instant>) -> bool {
```

The `_expected_expiry` is unused. The cached entry's own `expires_at` (set at cache time) is what gets checked via `entry.is_valid()`. This means the parameter is dead code. Not a correctness bug today (the cache works correctly via `CachedApproval::is_valid()`), but the function signature is misleading — it suggests the caller's `expires_at` is used for validation when it isn't.

**Fix:** Remove the `_expected_expiry` parameter from `is_approved_cached` entirely. Both call sites pass it but it's never used. Then in `cache_approval` for TimeBound, compute the correct `Some(Instant::now() + ttl)` at cache time (which is already what happens).

### C4. `strip_json_comments_simple` in executor.rs has a `prev` tracking bug

**File:** `crates/polyclaw/src/install/executor.rs` (the `strip_json_comments_simple` function)

```rust
let mut prev = '\0';
// ...
if c == '"' && prev != '\\' {
    in_string = false;
}
// ...
prev = c;
```

The `prev` variable tracks the previous character to detect escaped quotes (`\"`). But this only works for single-backslash escapes. A string like `"\\"` (a literal backslash) would set `prev = '\\'` on the first backslash, then when the second `\` is seen, `prev == '\\'` so the quote-check would fire. Then when `"` is seen after `\\`, `prev == '\\'` so it thinks it's escaped — but it's actually the end of the string.

The NZC version in `migration.rs` handles this correctly with an `escape_next` boolean flag.

**Fix:** Either use the NZC version (which is the right thing to do — see D1 below), or rewrite to use the `escape_next` pattern.

---

## Design Issues (should fix, affects correctness or maintainability)

### D1. Duplicated `strip_json_comments` implementations

`nonzeroclaw::onboard::migration::strip_json_comments` (correct, tested, handles edge cases)
vs
`polyclaw::install::executor::strip_json_comments_simple` (simpler, has the `prev` bug above)

The INSTALLER-IMPL-NOTES.md already calls this out. This is the canonical case for extraction into a shared crate.

**Fix:** Create `crates/claw-types/` (or `crates/polyclaw-shared/`) containing:
- `strip_json_comments` / `parse_json5_relaxed`
- `OpenClawInstallation` and detection logic
- `DetectedChannel`, `ChannelOwner`, `ChannelAssignment`

Both `nonzeroclaw` and `polyclaw` depend on this shared crate. This eliminates the duplication and the circular dependency problem.

### D2. `VaultAdapter::get_secret(key)` vs `VaultSecretConfig::bw_item_id` — naming gap

The `VaultAdapter` trait defines `get_secret(&self, key: &str)` where `key` is the _logical_ key name (e.g., `"anthropic_key"`). But `BitwardenCliAdapter` maintains an internal `item_ids: HashMap<String, String>` mapping logical keys to Bitwarden item IDs.

The `VaultManager::access_secret(key)` looks up the key in `VaultConfig::secrets` to get the policy, then calls `self.adapter.get_secret(key)` — passing the _logical_ key, not the `bw_item_id`.

Inside `BitwardenCliAdapter::get_secret`, it maps `key` → `item_ids[key]` → `bw_item_id` → `bw get password <bw_item_id>`.

**This works correctly** — the adapter owns the key→item_id mapping. But the trait's doc comment on `get_secret` says:

> The concrete meaning of `key` (e.g. a Bitwarden item ID, an env var name)
> is determined by the adapter implementation.

This is misleading. For `BitwardenCliAdapter`, `key` is _not_ the Bitwarden item ID — it's the logical config key. The doc should say: "The logical key name as configured in `vault.secrets.<key>`; the adapter resolves this to a backend-specific identifier."

### D3. `ClawAdapter` enum (installer) vs `AgentAdapter` trait (router) — concept collision

The installer's `ClawAdapter` enum (`install/model.rs`) defines adapter _kinds_ as data:
```rust
pub enum ClawAdapter {
    NzcNative,
    OpenClawHttp,
    OpenAiCompat { endpoint: String },
    // ...
}
```

The existing router's `AgentAdapter` trait (`adapters/mod.rs`) defines adapter _behavior_ as a trait:
```rust
pub trait AgentAdapter: Send + Sync {
    async fn dispatch(&self, text: &str) -> Result<String, AdapterError>;
    fn kind(&self) -> &str;
}
```

These are modeling the same concept from different angles — one is a type-level discriminator used at install time, the other is runtime dispatch. They'll eventually need to agree. Right now:

- Installer's `"nzc"` = router's `NzcNativeAdapter` / `NzcHttpAdapter`
- Installer's `"openclaw"` = router's `OpenClawHttpAdapter` / `OpenClawNativeAdapter`
- Installer's `"openai-compat"` = router's `OpenClawHttpAdapter` (probably)

The naming isn't consistent (`NzcNative` vs `nzc-http` vs `nzc-native`), and there's no formal mapping between the installer's adapter enum and the router's adapter registry.

**Fix (deferred, not blocking):** Define the canonical adapter kind strings in the shared crate, and have both the installer enum and the router's `build_adapter` function use the same constants.

### D4. `ChannelRouting` struct (wizard.rs) not connected to `ChannelAssignment` (migration.rs)

Session 2 defines `ChannelAssignment { channel: DetectedChannel, owner: ChannelOwner }` for channel ownership decisions.

Session 4 defines `ChannelRouting { channel_name: String, assigned_claw: String }` for the same concept — but with different types and no shared ancestry.

When the installer detects an existing OpenClaw installation and needs to route channels, it should use the `ChannelAssignment` data from the migration step, not collect fresh `ChannelRouting` data.

**Fix:** Unify these types in the shared crate. `ChannelRouting` should reference a `ChannelAssignment` or at least share a common channel identity type.

### D5. `MigrationWizardResult` is `pub(crate)` — can't be used by PolyClaw

**File:** `crates/nonzeroclaw/src/onboard/wizard.rs`

```rust
pub(crate) struct MigrationWizardResult {
```

The installer docs say PolyClaw needs this struct. But it's `pub(crate)`, so the polyclaw crate can't access it. The struct needs to be `pub` and re-exported from `onboard::mod.rs`.

### D6. `master_password` stored in memory for adapter lifetime

**File:** `crates/nonzeroclaw/src/vault/bitwarden.rs:164`

```rust
master_password: String,
```

The `BitwardenCliAdapter` holds the master password as a plain `String` for its entire lifetime. The VAULT-IMPL-NOTES.md acknowledges this ("NOTE: this is stored in memory for the lifetime of the adapter"). 

This isn't blocking, but the plan calls for reading it at unlock time and immediately discarding. Worth tracking as a follow-on fix alongside `zeroize` integration.

---

## Test Gaps (meaningful missing coverage)

### T1. No integration test for the vault→manager→adapter round-trip with config

The unit tests mock the adapter, and other tests mock the runner. There's no test that constructs a `VaultManager::from_config(VaultConfig)` with `backend = "bitwarden-cli"` and verifies the whole chain works (even with a MockBwRunner). This would catch wiring bugs between config parsing and adapter construction.

### T2. `apply_remote_config` stub not tested for rollback correctness with real JSON patching

The rollback test (`post_apply_health_check_failure_triggers_rollback`) works with the stub. When real JSON/TOML patching is implemented, the rollback test needs to verify that the _original_ file content (not just any backup) is restored.

### T3. No test for `ChannelApprovalRelay` with the callback

The `ChannelApprovalRelay` has a full async callback pipeline with `tokio::spawn`, oneshot channels, and timeout logic. There's no test exercising this. The `NoopApprovalRelay` is tested, but the actual relay that users will configure is untested.

**Fix:** Add tests that:
1. Create a `ChannelApprovalRelay` with a callback that immediately sends `Approved`
2. Create one with a callback that delays, then sends (test timeout)
3. Create one with a callback that drops the sender (should produce `TimedOut`)

### T4. Hegel property tests — assessment

The hegel property tests are **legitimate and well-designed**:

- `detect_channels_never_panics` — tests crash safety on arbitrary config shapes. Good.
- `config_migration_plan_never_panics` — same for migration planning. Good.
- `mapped_fields_always_have_known_nzc_paths` — verifies the field map is self-consistent. Good.
- `strip_comments_idempotent_on_clean_json` — verifies comment stripping preserves valid JSON. Good.
- `prop_shell_quote_safe` — verifies shell quoting produces valid POSIX-safe output. **Critical** — this is the test that validates injection safety.
- `prop_backup_unique_timestamps` and `prop_backup_contains_timestamp` — simpler but still useful for backup naming invariants.

These aren't trivial. The shell_quote property test in particular is doing real work.

### T5. Missing: `write_file` base64 round-trip test

The SSH `write_file` method uses base64 encoding to safely transmit arbitrary content:
```rust
let b64 = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
```

No test verifies that a round-trip (write → read) preserves the content for edge cases (empty strings, binary-like content, content with `\n`, `'`, `$`, etc.). The mock records the command but doesn't decode.

### T6. Missing: concurrent vault access test

`VaultManager` uses `tokio::sync::Mutex` for the approval cache. No test exercises concurrent access from multiple tasks. This is important because the primary use case is multiple tool calls accessing secrets simultaneously.

---

## Fork Strategy Assessment

### Current State

- **47 total commits** on `host-agent-v3` branch (currently checked out)
- **Origin:** `git@github.com-polyclaw:upstream-fork/polyclaw.git` — Brian's own repo
- **Upstream NZC:** The `nonzeroclaw` crate appears to be a local (non-forked) copy within the polyclaw monorepo
- **No upstream remote** for NZC — there's no `git remote` pointing to an NZC upstream

### What We've Added to NZC

The uncommitted changes (visible in `git status`) add:
1. **`vault/` module** (8 files, ~800 lines) — generic vault interface
2. **`onboard/migration.rs`** (~600 lines) — OpenClaw detection/migration
3. **Wizard changes** — OpenClaw migration step integrated into existing wizard
4. **Config schema change** — added `vault: VaultConfig` to `Config` struct

### Assessment

**This is Brian's own repo, not a fork of someone else's NZC.** The "fork strategy" question is simpler than the task assumed:

1. The vault interface (`VaultAdapter` trait, approval relay, `VaultManager`) is **generic and reusable**. If NZC were an external project, this would be a strong upstream contribution candidate.

2. The migration module is **PolyClaw-specific** (migrating from OpenClaw to NZC). It's in the NZC crate because it needs access to NZC's config types, but conceptually it's a PolyClaw installer concern.

3. The wizard changes are **NZC-specific** — they add a step to NZC's own onboarding flow.

### Recommendation

The monorepo structure is correct for now. Brian owns both crates. The risk isn't "upstream divergence" — it's **crate boundary hygiene**:

- Migration types (`OpenClawInstallation`, `DetectedChannel`, `ChannelAssignment`) are defined in NZC but needed by PolyClaw. These should live in a shared crate (see D1).
- The vault module is NZC-specific infrastructure that PolyClaw _uses_ through the install flow. This is fine where it is.
- If NZC ever becomes a published crate, the `pub(crate)` visibility on `MigrationWizardResult` needs to become `pub` with a considered public API.

**No urgent restructuring needed.** Extract the shared types crate, keep the vault in NZC, keep the installer in PolyClaw.

---

## Missing Scope Analysis

From `installer-open-questions.md`:

### Priority 1 — Breaks without it

1. **`apply_remote_config` real implementation** (the JSON/TOML patching stub)
   - **Impact:** Without this, the installer can go through all steps but the actual config change is a stub marker, not a real PolyClaw integration.
   - **Effort:** Medium. The JSON comment stripper exists (from NZC), the backup/rollback infrastructure is tested, the SSH wiring works. Just needs the actual patching logic.
   - **Can the bugfix session tackle this?** Yes. This is the single most important thing.

2. **Config write-back** (persist `ChannelRouting` to `~/.polyclaw/config.toml`)
   - **Impact:** The wizard collects channel routing but doesn't write it. PolyClaw can't route anything.
   - **Effort:** Low. Write the `[[routing]]` TOML entries from collected data.
   - **Bugfix session?** Yes.

### Priority 2 — Should have soon

3. **Shared crate extraction** (types + parsers)
   - Needed before the installer can actually call `parse_json5_relaxed` from NZC.
   - Medium effort. Important for D1/C4.
   - **Bugfix session?** Yes, can scope as "move types + parser to `claw-types` crate."

4. **`--yes` flag propagation** through wizard prompts
   - **Impact:** Scripted/CI installs can't skip confirmations.
   - **Effort:** Low.
   - **Bugfix session?** Yes.

5. **SSH key validation** (file exists, permissions 0600)
   - **Impact:** Bad error messages when key path is wrong.
   - **Effort:** Low.
   - **Bugfix session?** Yes.

### Priority 3 — Separate scope

6. **System user/account provisioning** — SSH in, create user, deploy keys
   - This is a full feature, not a bugfix.

7. **Service install/management** — write systemd units, restart services
   - Full feature.

8. **Clash/permissions policy sync** — deploy scoped policies to each claw
   - Full feature, needs its own design doc.

9. **Kill/restart handling** — detect if config change requires service restart
   - Medium complexity, separate scope.

### Order

Bugfix session priorities: **1 → 2 → 3 → 4 → 5** (in that order). Items 6-9 are follow-on sessions.

---

## Recommended Bugfix Session Plan

### One combined bugfix session (scope: 1 day)

1. **Real `apply_remote_config`** — replace stub with actual JSON/TOML patching
   - For OpenClaw: add `hooks.enabled`, `hooks.token` to JSON
   - For NZC: add `[polyclaw]` section to TOML
   - Use `parse_json5_relaxed` from NZC (requires step 3 or a temporary copy)

2. **Persist channel routing** — write `[[routing]]` entries to PolyClaw config
   - Read existing config → merge routing → write back

3. **Extract shared crate** — `crates/claw-types/`
   - Move: `strip_json_comments`, `parse_json5_relaxed`, `OpenClawInstallation`, `DetectedChannel`, `ChannelOwner`, `ChannelAssignment`
   - Update imports in both NZC and PolyClaw

4. **Fix C1** (remove `Clone` from `Secret`/`SecretValue`)
5. **Fix C2** (pass master password via stdin or child env, not argv)
6. **Fix C3** (remove dead `_expected_expiry` parameter)
7. **Fix C4** (remove duplicated `strip_json_comments_simple`, use shared crate version)
8. **Wire `--yes` flag** through wizard `Confirm` prompts
9. **SSH key path validation** — check exists + permissions before attempting SSH

### Defer to follow-on sessions

- `ChannelApprovalRelay` wiring to Signal/Telegram (needs channel layer work)
- `zeroize` crate integration (needs audit of all secret-holding types)
- System user provisioning, service management, Clash policy sync
- Vault approval persistence across restarts
- `bw` item upsert (update existing items)
- `MigrationWizardResult` visibility fix (depends on shared crate decision)
- Integration test for `VaultManager::from_config` full chain

---

## Overall Assessment

**The code is solid.** The design is thoughtful, the safety invariants are real (not theater), and the test coverage is unusually good for a 4-session sprint. Specific strengths:

1. **Trait-based dependency injection everywhere.** `BwRunner`, `SshClient`, `HealthChecker`, `AsyncLlmFn`, `ApprovalRelay` — all mockable, all tested through mocks. This is the right architecture.

2. **Safety protocol is real, not cargo-cult.** Backup-before-write, verify-backup-exists, health-check-after-apply, automatic-rollback — each step is implemented and tested. The rollback test with sequential mock health responses is particularly well done.

3. **Secret handling is mostly correct.** Debug redaction, `Drop` zeroing (best-effort), `NoopVaultAdapter` for disabled backends. The `Clone` issue (C1) is the main gap.

4. **`shell_quote` is actually safe.** The POSIX `'\''` idiom is correct. The property tests verify it on arbitrary printable ASCII. The only path to injection would be if a caller interpolated user input _outside_ of `shell_quote`, and the code consistently quotes all user-supplied strings.

5. **The hegel property tests test meaningful invariants**, not just "doesn't crash." The shell_quote test is genuinely important for security.

**What needs attention before merge:**

- Fix C1 (Clone on Secret) and C2 (master password on CLI) — these are real security issues
- Fix C4 (duplicated parser with bug) — either via shared crate or quick replacement
- Clean up C3 (dead parameter) — trivial but the code is misleading as-is
- Real `apply_remote_config` is the highest-value follow-on work

**Overall verdict:** Good to merge after fixing C1, C2, C4. The stubs and missing features are properly documented and don't create correctness issues — they just limit what the installer can actually do. The architecture is sound and the follow-on work has a clear path.
