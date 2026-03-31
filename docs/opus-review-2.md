# Opus Review 2 — 2026-03-30

_Reviewer: Claude Opus 4.6 (subagent session, follow-up to opus-review.md)_
_Scope: Bugfix verification (C1–C4, S1, D2), pre-existing test failure diagnosis, fork strategy, property test opportunities, TODO/stub triage_

---

## Bugfix Verification

### C1 — `Secret` / `SecretValue` / `SessionToken` no longer `Clone` ✅ CORRECT

The fix is exactly right. `#[derive(Clone)]` is removed from all three types, with clear doc comments explaining the rationale. The `Drop` impl on `Secret` (which zeros the buffer via `unsafe { self.value.as_bytes_mut() }`) now actually provides its intended guarantee — no untracked copies exist.

Verified that no call sites were broken: the BUGFIX-NOTES correctly observed that the `.clone()` calls in tests/adapters were on `String` fields, not on the vault types themselves.

**One remaining issue:** `SecretValue` and `SessionToken` lack `Drop` impls for zeroing. `Secret` zeros its buffer on drop, but `SecretValue.inner` and `SessionToken.token` are plain `String`s that will be freed but not zeroed. This is internally consistent (the VAULT-IMPL-NOTES acknowledge `zeroize` integration as follow-on work), but worth noting: removing `Clone` without adding `Drop` zeroing doesn't actually improve security for these two types — it just prevents future problems.

**Verdict: Fix is correct. No new issues.**

---

### C2 — Master password via stdin ⚠️ INCORRECT FLAG

The fix correctly identifies the security problem (password in `/proc/<pid>/cmdline`) and correctly implements the stdin pipe pattern — the `tokio::process::Command` stdin write + `wait_with_output()` is textbook correct for pipe-then-close:

```rust
let mut child = tokio::process::Command::new(bw_path)
    .args(["unlock", "--raw", "--passwordstdin"])
    .stdin(std::process::Stdio::piped())
    // ...
```

The stdin handle is implicitly closed when `child.stdin` is dropped after the `write_all`, and `wait_with_output()` properly waits for the process to consume all input and exit. **The pipe lifecycle is correct — `bw` will not hang.**

**However, `--passwordstdin` is not a real `bw` CLI flag.**

The Bitwarden CLI (`bw unlock`) supports:
- `bw unlock [password]` — positional argument (insecure, visible in ps)
- `bw unlock --passwordenv <ENV_VAR_NAME>` — reads from named env var
- `bw unlock --passwordfile <path>` — reads from file

There is no `--passwordstdin` flag. Running `bw unlock --raw --passwordstdin` will produce an error like `error: unknown option '--passwordstdin'`.

**This means the C2 fix compiles and passes tests (because the `MockBwRunner` intercepts the call before it hits a real `bw` process), but will fail at runtime against an actual Bitwarden CLI.**

**Recommended fix:** Use `--passwordenv` with a child-process-only env var:

```rust
async fn unlock(&self, bw_path: &str, master_password: &str) -> VaultResult<String> {
    let output = tokio::process::Command::new(bw_path)
        .args(["unlock", "--raw", "--passwordenv", "BW_UNLOCK_PW"])
        .env("BW_UNLOCK_PW", master_password)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| { /* ... */ })?
        .wait_with_output()
        .await
        .map_err(VaultError::Io)?;
    // ...
}
```

The child env is set only for the spawned process, not the parent. On Linux, `/proc/<pid>/environ` is protected by the same permissions as the process itself (only the process owner or root can read it), unlike `/proc/<pid>/cmdline` which is world-readable. This is a genuine security improvement over the positional-argument approach.

Alternative: `--passwordfile /dev/stdin` combined with the existing stdin pipe. This is a hack but works because `bw` will `read_to_string` the file path, and `/dev/stdin` routes to the process's stdin fd.

**Verdict: Fix intent is correct, security reasoning is sound, pipe lifecycle is correct, but `--passwordstdin` doesn't exist as a bw flag. Runtime breakage guaranteed. Use `--passwordenv` with child env instead.**

**Test quality:** The `CapturingBwRunner` test is well-designed — it verifies that the password is NOT embedded in `bw_path` and IS passed as a separate parameter. The test will continue to pass after switching to `--passwordenv` since it validates the separation of concerns, not the specific flag name. Good test.

---

### C3 — Dead `_expected_expiry` parameter removed ✅ CORRECT

Clean removal of the unused parameter from `is_approved_cached`. Both call sites (Session and TimeBound branches) now call `self.is_approved_cached(key)` without the phantom parameter.

The `cache_approval` call correctly passes `Some(expires_at)` for TimeBound and `None` for Session. The `CachedApproval::is_valid()` check correctly uses the `expires_at` set at cache time.

The test suite for `VaultManager` is comprehensive: auto/per-use/session/time-bound policies are all tested, cache invalidation is tested, and deny/timeout relays are tested. The `CountingRelay` pattern for verifying relay call counts is well-designed.

**One subtle issue in TimeBound test:** `test_time_bound_policy_expires_and_re_approves` uses `invalidate_approval` to simulate TTL expiry rather than actually sleeping. This is fine for unit testing (avoids flaky timing), but means there's no test that exercises the `CachedApproval::is_valid()` time check path. The `expired_session_triggers_relock` test in `bitwarden.rs` does exercise real time-based expiry for the session token, so the pattern is validated there, just not at the VaultManager level.

**Verdict: Fix is correct. No new issues.**

---

### C4 + D1 — JSON5 comment stripping fixed ✅ CORRECT

The new `json5.rs` module in `polyclaw/src/install/` correctly implements the `escape_next` boolean pattern:

```rust
if ch == '\\' {
    escape_next = true;
    out.push(ch);
}
```

vs the old buggy `prev` tracking:
```rust
if c == '"' && prev != '\\' { /* wrong for "\\" */ }
```

The `escape_next` approach is provably correct for all single-character escape sequences: `\\`, `\"`, `\n`, `\t`, etc. After a backslash, the next character is unconditionally emitted without checking whether it's a string terminator or comment starter.

**Regression test is meaningful:**
```rust
fn escaped_backslash_before_closing_quote() {
    let input = r#"{"k": "\\"}"#;
    let v = parse_json5_relaxed(input).unwrap();
    assert_eq!(v["k"], "\\");
}
```

This directly exercises the `"\\"` case that was broken. Not coverage theater — this is the exact input class that triggered the bug.

Additional tests cover: line comments, block comments, URLs in strings (false positive prevention), escaped quotes, comment after escaped backslash, empty input, and plain JSON passthrough. Good coverage.

**Verdict: Fix is correct. Tests are meaningful. Duplication is documented as intentional (pending `claw-types` extraction).**

---

### S1 — `apply_remote_config` real JSON patching ✅ CORRECT (with caveats)

The OpenClaw config patching flow is well-structured:
1. Read via SSH → 2. Strip comments → 3. Parse JSON → 4. Upsert hooks entry → 5. Write via SSH → 6. Read-back verify

**Token preservation is correct:** existing tokens are preserved on re-run via the `existing_token` check. New entries get a generated token. This makes the operation idempotent.

**`or_insert` vs `or_insert_with` on `hooks.enabled`:** The code uses `entry("enabled").or_insert(json!(true))` — this only sets `enabled: true` if the key doesn't exist. If the user has explicitly set `hooks.enabled = false`, it's preserved. This is the correct behavior: don't forcibly override user choices.

**Token generation weakness is documented** but still a concern: `generate_hook_token` uses `DefaultHasher` seeded with time + PID, which is deterministic given the same inputs and not cryptographically random. The function produces a 48-char hex string from three cascaded hashes. Two calls in the same millisecond from the same PID produce identical tokens. This is flagged in the docstring but worth reiterating: replace with `getrandom` before any production use.

**NZC stub is appropriately limited:** `patch_nzc_config_stub` does a string-contains check for `[polyclaw]` and appends if absent. It's idempotent and clearly marked as a stub.

**Verdict: Implementation is correct. Token generation weakness is known. Tests adequately cover the patching logic.**

---

### D2 — `ClawAdapter` → `ClawKind` rename ✅ CORRECT

Simple rename, correctly applied across all affected files. The new name (`ClawKind`) avoids confusion with the runtime `AgentAdapter` trait. Good.

---

## Pre-existing Test Failure: Root Cause + Fix

### The failing test

```
test_switch_updates_routing_for_subsequent_messages
left: Some("custodian")
right: Some("librarian")
```

Line 1182: `assert_eq!(h.active_agent_for("brian"), Some("librarian".to_string()))` — this is the FIRST assertion in the test, which expects the default agent. It gets `"custodian"` instead.

### Root cause: shared filesystem state between test runs

`CommandHandler::new()` calls `load_active_agents()` which reads `~/.polyclaw/state/active-agents.json`. This is a real file on disk, shared across all test invocations.

The test `test_switch_updates_active_agent_for_identity` (which runs before or concurrently) calls `h.handle_switch("!switch custodian", "brian")`, which calls `save_active_agents` and writes `{"brian": "custodian"}` to that file.

When `test_switch_updates_routing_for_subsequent_messages` then creates a fresh `CommandHandler` via `make_handler()`, the constructor loads the persisted state file and finds `brian → custodian`. The in-memory override takes precedence over the config default, so `active_agent_for("brian")` returns `"custodian"` instead of `"librarian"`.

**Current state of the file on this machine:**
```json
{"brian": "custodian"}
```

This confirms the diagnosis. Delete this file and all switch-related tests pass. Run them again and the file gets recreated, causing the next run to fail if test ordering changes.

### Fix

The tests need filesystem isolation. Two options:

**Option A (recommended): Test-scoped temp directory override**

Add a field to `CommandHandler` for the state directory, and use it in `load_active_agents` / `save_active_agents`:

```rust
pub struct CommandHandler {
    // ... existing fields ...
    /// Directory for state persistence. Defaults to ~/.polyclaw/state/.
    /// Overridable for tests.
    state_dir: PathBuf,
}

impl CommandHandler {
    pub fn new(config: Arc<PolyConfig>) -> Self {
        Self::with_state_dir(config, default_state_dir())
    }

    pub fn with_state_dir(config: Arc<PolyConfig>, state_dir: PathBuf) -> Self {
        let active_agents = load_active_agents_from(&state_dir);
        // ... rest of constructor ...
        Self {
            state_dir,
            // ...
        }
    }
}
```

Then in tests:
```rust
fn make_handler() -> CommandHandler {
    let tmp = tempfile::tempdir().unwrap();
    CommandHandler::with_state_dir(Arc::new(make_config()), tmp.path().to_path_buf())
}
```

**Option B (quick fix): Set `$HOME` per-test**

Override `HOME` env var to a temp directory in `make_handler()`. Fragile (affects all path lookups) and racy (env is process-wide). Not recommended.

**Option C (quickest fix): Clean the state file in `make_handler()`**

```rust
fn make_handler() -> CommandHandler {
    // Clear any persisted state from previous test runs
    let _ = std::fs::remove_file(state_file_path());
    CommandHandler::new(Arc::new(make_config()))
}
```

This is a one-liner that fixes the immediate problem but leaves a race condition between parallel tests. Good enough for now; Option A is the proper fix.

**Recommendation: Option C immediately to unblock CI, then Option A as a follow-on refactor.**

---

## Fork Strategy Assessment

### Structure

| Component | Location | Relationship |
|-----------|----------|-------------|
| polyclaw-mono | `/root/projects/polyclaw-mono` | Brian's monorepo. Contains `nonzeroclaw` and `polyclaw` crates. Not a fork. |
| nonzeroclaw fork | `/root/projects/nonzeroclaw` | Fork of `zeroclaw-labs/zeroclaw` at `upstream-fork/zeroclaw` |
| matrix-rust-sdk fork | git dep in `Cargo.toml` | `upstream-fork/matrix-rust-sdk`, pinned by commit |

### Fork analysis: `/root/projects/nonzeroclaw`

**Remotes:**
- `origin` → `git@github.com:upstream-fork/zeroclaw.git` (Brian's fork)
- `upstream` → `https://github.com/zeroclaw-labs/zeroclaw.git` (canonical)

**Branch:** `polyclaw-patches` is the active branch. It has **9 commits** ahead of the merge base (commit `314e1d3`, the channel-approval-manager merge from upstream).

**Commit breakdown:**

| Commit | Author | Description | Upstreamable? |
|--------|--------|-------------|---------------|
| `a40218a` | Librarian | polyclaw: initial patches (anthropic fixes, approval flow, openai_compat, clash crate) | Partly — anthropic fixes yes, polyclaw-specific no |
| `e6d286f` | Librarian | chore: remove nested zeroclaw-review git repo, add to .gitignore | Infra-specific |
| `4650e5d` | Librarian | chore: regenerate Cargo.lock, fix nonzeroclaw:: → zeroclaw:: in main.rs | Infra-specific |
| `6f47e20` | imu | fix(config): support socks proxy scheme for Clash Verge | ✅ Upstreamable |
| `94aba32` | Sandeep (Claude) | fix(channel): resolve multi-room reply routing regression | ✅ Upstreamable |
| `73faf6c` | Sandeep (Claude) | style: cargo fmt to channel routing fix | ✅ Upstreamable |
| `fea93a9` | I329802 | fix(providers): send MCP image tool results as native Anthropic image blocks | ✅ Upstreamable |
| `7f428c6` | I329802 | style: cargo fmt to Anthropic MCP image blocks | ✅ Upstreamable |
| `cb7bf6e` | panviktor | fix(provider): harden Anthropic vision — MIME validation, cache-control walk | ✅ Upstreamable |

**Summary:** 6 of 9 commits are upstream bug fixes that happened to land on this branch first. 3 are polyclaw-specific infrastructure. **Upstream is 498 commits ahead** of `origin/main` (Brian's fork main isn't tracking latest upstream).

### Is polyclaw-mono's `nonzeroclaw` crate the same as the fork?

**No.** The `polyclaw-mono/crates/nonzeroclaw/` is a **separate codebase** — a reimplementation/fork-of-a-fork. It:
- Has its own `Cargo.toml` with `name = "nonzeroclaw"` (not `zeroclaw`)
- Lists `zeroclawlabs = { version = "0.4", optional = true }` as an optional dep (passthrough modules)
- Contains the vault module, migration module, and wizard changes — none of which exist in `/root/projects/nonzeroclaw`
- Is ~436 lines in `lib.rs` with a different module structure

The `/root/projects/nonzeroclaw` fork is the zeroclaw codebase with patches. The `polyclaw-mono/nonzeroclaw` crate is a new, separate implementation that can optionally depend on the upstream `zeroclawlabs` crate for passthrough.

### Rebase risk

**High for `/root/projects/nonzeroclaw`:** 498 commits behind upstream. The 6 upstreamable fixes may already be fixed differently upstream. Rebase will be painful. The 3 infra commits touch `main.rs` and `Cargo.lock` which have certainly changed.

**Low for `polyclaw-mono/nonzeroclaw`:** Since it's a separate crate that optionally depends on `zeroclawlabs = "0.4"`, upstream releases don't break it. The risk is API surface drift: if upstream changes the types that NZC wraps in passthrough modules, those modules need updating.

**matrix-rust-sdk fork:** Unknown risk without examining the fork. Pinned by commit, so it's stable until someone bumps it.

### Recommendation

1. **Upstream the 6 bug-fix commits** from `polyclaw-patches` to zeroclaw-labs via PRs. They're clean, authored by various contributors, and fix real bugs. This reduces fork maintenance burden and gives back to the project. Do this before the rebase becomes impossible.

2. **Rebase `polyclaw-patches` onto latest `upstream/master`** after the PRs are merged. This will likely conflict on `Cargo.lock` and the `main.rs` namespace changes. Budget a half-day.

3. **For `polyclaw-mono/nonzeroclaw`:** keep it as-is. It's a separate crate, not a fork. The optional `zeroclawlabs` passthrough dep means it can track upstream releases at its own pace. When `zeroclawlabs` 0.5 ships, bump the dep and update passthrough modules.

4. **For `matrix-rust-sdk`:** audit why the fork exists (likely a fix not yet merged upstream). If the fix has been merged, drop the fork and use the upstream crate. If not, submit the fix upstream and maintain the pin until it's merged.

5. **Don't restructure.** The current arrangement (separate monorepo + upstream fork with patches) is correct for the relationship. The monorepo owns the novel code; the fork tracks upstream with cherry-picks. Merging them would create a maintenance nightmare.

---

## Property Test Opportunities (10 specific suggestions)

### 1. `strip_json_comments` round-trip: valid JSON is preserved

**File:** `crates/polyclaw/src/install/json5.rs` — `strip_json_comments`
**Property:** For any valid JSON string `s` (no comments), `serde_json::from_str(&strip_json_comments(s))` produces the same value as `serde_json::from_str(s)`.
**Why unit tests miss it:** The existing `plain_json_unchanged` test uses a single hardcoded input. A property test would explore strings with backslashes, nested quotes, unicode, empty objects, deeply nested structures — any of which could confuse the state machine.

### 2. `strip_json_comments` never adds content

**File:** `crates/polyclaw/src/install/json5.rs` — `strip_json_comments`
**Property:** `strip_json_comments(s).len() <= s.len()` for all inputs. Comment stripping should only remove characters, never add them.
**Why unit tests miss it:** No existing test checks length invariants. A pathological input (e.g., unmatched `/*` at end of input) could theoretically trigger unexpected behavior in the while loop.

### 3. `shell_quote` ∘ POSIX eval = identity

**File:** `crates/polyclaw/src/install/ssh.rs` — `shell_quote`
**Property:** For any ASCII string `s`, `eval echo $(shell_quote(s))` produces exactly `s`. This is strictly stronger than the existing structural test.
**Why unit tests miss it:** The existing hegel test (`prop_shell_quote_safe`) verifies structural properties (starts/ends with `'`, uses `'\''` for embedded quotes) but doesn't verify semantic correctness — that a real shell interprets the quoted string as the original value. A property test with `std::process::Command::new("sh").args(["-c", &format!("printf '%s' {}", quoted)])` would catch semantic bugs that structural tests miss.

### 4. `resolve_channel_sender` — exactly one match per (channel, id) pair

**File:** `crates/polyclaw/src/auth.rs` — `resolve_channel_sender`
**Property:** For any `PolyConfig` where no two identities share the same `(channel, id)` alias, `resolve_channel_sender` returns at most one result AND it's the correct identity.
**Why unit tests miss it:** Unit tests use a fixed config with 2 identities. They don't test what happens with duplicate aliases, empty aliases, aliases with special characters, or configs with 100+ identities. A property test generating random configs would catch precedence bugs (e.g., first-match vs wrong-match).

### 5. `patch_openclaw_config` preserves existing config fields

**File:** `crates/polyclaw/src/install/executor.rs` — `patch_openclaw_config`
**Property:** For any valid JSON config object `c` and any claw_name/endpoint, all keys in `c` are present in the output of `patch_openclaw_config(c, name, endpoint)` (except `hooks` which is modified).
**Why unit tests miss it:** Existing tests use small, fixed configs. A property test with arbitrary JSON objects would catch field-dropping bugs (e.g., `serde_json` serialization losing keys due to `as_object_mut` aliasing).

### 6. `active_agent_for` defaults are consistent with config

**File:** `crates/polyclaw/src/commands.rs` — `active_agent_for`
**Property:** For any `PolyConfig` and any identity in `config.routing`, `active_agent_for(identity)` returns either the `default_agent` (if no switch has occurred) or a value from `config.agents[].id`.
**Why unit tests miss it:** Current tests use a fixed 2-agent config. A property test generating random configs would verify the invariant holds for arbitrary numbers of agents, identities, and routing rules.

### 7. `backup_filename` parsing round-trip

**File:** `crates/polyclaw/src/install/model.rs` — `backup_filename`
**Property:** For any `(path, timestamp)`, the backup filename can be parsed back to extract the original path and timestamp: `split at last ".bak."` recovers both.
**Why unit tests miss it:** The existing property tests check uniqueness and prefix, but not recoverability. If the original path contains `.bak.` (e.g., `/path/to/file.bak.old/config.json`), parsing becomes ambiguous. A property test would discover this edge case.

### 8. `SecretPolicy` enum exhaustiveness in `access_secret`

**File:** `crates/nonzeroclaw/src/vault/manager.rs` — `access_secret`
**Property:** For every variant of `SecretPolicy`, the relay is called the expected number of times after N accesses: Auto→0, PerUse→N, Session→1, TimeBound→1 (within TTL).
**Why unit tests miss it:** Each variant is tested individually. A property test parameterized over `(policy, access_count)` pairs would verify the invariant holds for arbitrary access counts and catch any off-by-one or cache coherence bugs.

### 9. `check_version_compatibility` — known versions are always Compatible

**File:** `crates/polyclaw/src/install/model.rs` — `check_version_compatibility`
**Property:** For every `(adapter, version)` in the hardcoded `OPENCLAW_COMPATIBLE_VERSIONS` and `NZC_COMPATIBLE_VERSIONS` lists, the result is `Compatible`. For any string NOT in those lists, the result is `Unknown` (never `Incompatible`, since that variant is reserved).
**Why unit tests miss it:** The tests check 2 representative versions. A property test generating arbitrary strings would verify the function never returns `Incompatible` (which would be a bug, since no incompatibility rules are defined yet).

### 10. `CachedApproval::is_valid()` monotonicity

**File:** `crates/nonzeroclaw/src/vault/manager.rs` — `CachedApproval::is_valid`
**Property:** For a `CachedApproval` with `expires_at = Some(t)`, if `is_valid()` returns `false` at time T, it returns `false` for all T' > T (validity is monotonically decreasing).
**Why unit tests miss it:** The existing tests use `invalidate_approval` to simulate expiry. A time-based property test (using `tokio::time::pause` + `advance`) would verify that once expired, an approval stays expired — catching bugs where the cache entry is accidentally refreshed without relay involvement.

---

## TODO/Stub Prioritization

### From `crates/polyclaw/src/install/`

| Location | What | Gap Type | Priority | Effort |
|----------|------|----------|----------|--------|
| `migration_types.rs:16` | TODO: extract to `claw-types` shared crate | Completeness (duplication) | P1 | Medium |
| `json5.rs:14` | TODO: extract to `claw-types` shared crate | Completeness (duplication) | P1 | Medium |
| `executor.rs:617` | Comment: "stubbed — expand per adapter" | Documentation, not a gap | — | — |
| `executor.rs:697` | TODO: implement real TOML patching for NZC | Correctness gap — NZC installs are broken | P0 | Medium |
| `executor.rs:838` | `patch_nzc_config_stub` implementation | Correctness gap — appends raw TOML without parsing | P0 | Medium |

### From `crates/nonzeroclaw/src/vault/`

No TODOs found in vault code. The `zeroize` integration and `master_password` lifetime improvements are mentioned in VAULT-IMPL-NOTES but not as inline TODOs.

### From `crates/nonzeroclaw/src/onboard/migration.rs`

| Location | What | Gap Type | Priority | Effort |
|----------|------|----------|----------|--------|
| `migration.rs:399` | `apply_migration_changes` is stubbed | Correctness — migration wizard can plan but can't execute | P0 | Large |
| `migration.rs:447-468` | `StubLlmFn` returns placeholder content | Completeness — memory migration produces dummy output | P1 | Medium |
| `migration.rs:475` | `FailingLlmFn` stub for error tests | Test infrastructure — fine as-is | — | — |
| `migration.rs:692` | migrate-memory command stub section | See 447-468 above | P1 | Medium |
| `migration.rs:729` | TODO: replace StubLlmFn with NZC's real provider client | Completeness — dry-run works, real mode needs real LLM | P1 | Medium |

### Prioritized list

**P0 — blocks real use:**
1. **NZC TOML patching** (`executor.rs:697`): The installer can't actually configure NZC targets. Anyone trying to install PolyClaw with NZC agents hits a stub that appends raw text to TOML. Use `toml_edit` crate for proper in-place TOML modification.
2. **`apply_migration_changes` stub** (`migration.rs:399`): The migration wizard can detect channels, plan config changes, and generate a migration report — but it can't actually apply the changes. This means the `nzc migrate` command goes through the entire wizard flow and then does nothing.

**P1 — needed soon:**
3. **`claw-types` shared crate** (multiple locations): Two copies of `strip_json_comments`, two copies of migration types. Growing duplication that will cause subtle divergence bugs.
4. **Real LLM integration for memory migration** (`migration.rs:729`): The memory migration command works in dry-run mode with `StubLlmFn`. For real use, it needs to call NZC's configured LLM provider to summarize/restructure the memory files.

**P2 — nice to have:**
5. **`zeroize` crate integration**: Replace the manual `unsafe` zeroing in `Secret::drop` with the `zeroize` crate's compiler-fence-backed implementation. Add `Drop` impls to `SecretValue` and `SessionToken`.
6. **`generate_hook_token` CSPRNG**: Replace `DefaultHasher` with `getrandom`.

---

## Overall: What to tackle next

In priority order:

1. **Fix C2 (`--passwordstdin` → `--passwordenv`)** — this is a runtime breakage in the vault unlock path. 15-minute fix. Change the flag, update the docstring, tests already pass.

2. **Fix the test failure** — `rm ~/.polyclaw/state/active-agents.json` immediately, then add the `make_handler()` cleanup (Option C). 5-minute fix. Follow up with Option A (state_dir injection) when refactoring commands.rs.

3. **NZC TOML patching** — the stub is the biggest functional gap. Add `toml_edit` dep, implement proper `[polyclaw]` section insertion/update. Half-day.

4. **`apply_migration_changes` real implementation** — the other P0 stub. This is larger (writes multiple files, restarts services). Separate session.

5. **`claw-types` crate extraction** — prevents the duplication from getting worse. Can be done incrementally: start with `strip_json_comments` and the migration types, wire both crates to use it.

6. **Upstream the 6 bug-fix commits** from the nonzeroclaw fork — reduces maintenance burden, contributes back to the project.

The codebase is in good shape. The architecture is sound, the safety invariants are real, and the test coverage is strong. The two immediate blockers are the `--passwordstdin` flag (doesn't exist) and the test state leakage (filesystem). Everything else is incremental improvement on a solid foundation.
