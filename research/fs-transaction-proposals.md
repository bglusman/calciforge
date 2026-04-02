# Filesystem Transaction Interface Design Proposals

_Research date: 2026-03-30_
_Context: Three concrete interface proposals for the NZC/PolyClaw transaction system_

---

## Background

The goal is to give agents a way to say "I'm about to do something dangerous; protect me." The interface (how the agent expresses this) is separate from the backend (how the protection works). All three proposals share the same backend options (backup copy, ZFS snapshot, jai/overlayfs).

---

## Proposal A: NZC Tool-Call Interface

### The API

```
// Start a transaction. Returns a handle.
transaction_start(
    label: string,            // Human-readable label, e.g. "edit openclaw config"
    backend: "auto" | "backup-copy" | "jai" | "zfs",  // default: "auto"
    scope: list[path]?,       // Files/dirs this transaction covers (optional hint)
) → TxnHandle { id: UUID, label: string, backend: string, created_at: timestamp }

// Commit all changes made while transaction was active
transaction_commit(
    handle: TxnHandle,
    message: string?,         // Optional commit message for audit log
) → CommitResult { ok: bool, files_changed: list[path], backup_paths: list[path]? }

// Roll back all changes
transaction_rollback(
    handle: TxnHandle,
    reason: string?,
) → RollbackResult { ok: bool, files_restored: list[path] }

// List active transactions (useful for crash recovery)
transaction_list() → list[TxnHandle]

// Inspect what a transaction changed (before committing)
transaction_diff(handle: TxnHandle) → Diff { changed: list[FileDiff] }
```

### How the Agent Uses It in Practice

**Worked example: editing OpenClaw config**

```
// Agent is about to edit openclaw.json for the PolyClaw adapter installation

txn = transaction_start(
    label="polyclaw adapter installation - openclaw.json edit",
    backend="auto",
    scope=["/root/.openclaw/openclaw.json"]
)

// NZC backs up openclaw.json before the next write

write("/root/.openclaw/openclaw.json", new_config_content)
// ... validate config ...
exec("openclaw gateway restart")
// ... health check: poll /health for 30s ...

if health_check_passes:
    transaction_commit(txn, message="Added polyclaw channel adapter")
    // Backup files cleaned up or kept per config
else:
    transaction_rollback(txn, reason="Gateway failed to start after config edit")
    // openclaw.json restored from backup
    exec("openclaw gateway restart")  // Restart with original config
```

**Key behavior: auto-wrapping**

When a transaction is active, NZC intercepts all write/edit/exec-with-writes tool calls:
- If the write touches a path in the active transaction's scope → proceeds normally, changes tracked
- If `transactions.intercept = true` (default off) → ALL writes during transaction are intercepted and backed up even if not in declared scope

The agent doesn't need to call `transaction_start` before every single write — the transaction covers a logical operation group.

### Auto-Wrapping Decision

**Should `write` and `edit` tool calls auto-wrap in a transaction if one is active?**

Yes, with the backup-copy backend. When `transaction_start` is called:
- NZC creates a transaction record with an ID
- Every subsequent `write`/`edit` call checks: is there an active transaction? If so, back up the file first (if not already backed up in this transaction)
- On `transaction_commit`: mark backups as committed (optionally delete or archive them)
- On `transaction_rollback`: restore all backed-up files

This is the right behavior. It makes `transaction_start` genuinely useful rather than just a declaration.

**Should file editing be BLOCKED unless inside a transaction?**

Configurable via `transactions.mode`:
- `"off"` — no transaction enforcement, no auto-backup (default for now)
- `"suggest"` — write to sensitive paths emits a warning if no transaction is active, but proceeds
- `"require"` — writes to sensitive paths are BLOCKED without an active transaction

The `require` mode is the safety-first option for production agents. The `suggest` mode is good for development/migration.

**What paths count as "sensitive" for suggest/require mode?**

Default sensitive path patterns (configurable):
```toml
[transactions.sensitive_paths]
patterns = [
    "~/.openclaw/**",
    "/etc/**",
    "~/.config/**",
    "~/.jai/**",
    # NZC workspace directories are explicitly EXCLUDED — agents write there freely
]
```

### Granularity: Per-File vs Per-Operation-Set

**Per-operation-set is correct.** Here's why:

A config edit is usually not "write file X." It's "write file X, restart service Y, verify it came up, then call it done." The transaction should cover the entire operation group, not individual file writes.

Per-file transactions are:
1. Too fine-grained — the agent would have to call `transaction_start`/`transaction_commit` for every individual file write
2. Philosophically wrong — the meaningful unit is "did this operation succeed or fail?"

The transaction wraps the sequence:
```
txn = start()
  write config
  restart service
  health check
  if ok: commit
  else: rollback
```

### Composition: Nesting Transactions

**Supported but limited:**

```
outer = transaction_start("full install")
    inner = transaction_start("step 1: edit config")
        write(config_file)
    transaction_commit(inner)  // marks step 1 as done within outer

    inner2 = transaction_start("step 2: add channel")
        write(channel_config)
    // inner2 crashes here
    transaction_rollback(inner2)
    // outer still active; can continue or roll back everything

transaction_rollback(outer)  // rolls back everything including committed inner
```

With the backup-copy backend: nesting works by tracking the original pre-transaction backup separately from intermediate commits. Rolling back outer restores the original backup, ignoring inner commits.

With ZFS backend: nested transactions correspond to nested snapshots — this works cleanly.

With jai backend: nesting is awkward (you can't nest overlays easily). Simplify: outer transaction uses backup copy even if inner uses jai.

**Cross-SSH nesting:** Not supported in v1. Cross-SSH operations inside a transaction get a warning: "remote changes are not covered by local transaction." See `fs-transaction-ssh-boundaries.md`.

### Crash Mid-Transaction

NZC persists the transaction log to SQLite (already available) at transaction start:
```sql
CREATE TABLE transactions (
    id TEXT PRIMARY KEY,
    label TEXT,
    backend TEXT,
    started_at TEXT,
    state TEXT,  -- 'active' | 'committed' | 'rolled_back'
    files_json TEXT  -- JSON list of {original_path, backup_path}
);
```

On crash:
1. NZC starts up, queries `SELECT * FROM transactions WHERE state = 'active'`
2. For each orphaned transaction: alerts the user/agent "Transaction 'X' was interrupted. Run `transaction_rollback(id)` to restore originals, or `transaction_commit(id)` if the operation succeeded."
3. Does NOT auto-rollback on startup (could be wrong if operation actually succeeded before crash)
4. Backups remain intact until explicitly committed or rolled back

### Configuration Surface

```toml
[transactions]
mode = "off"           # "off" | "suggest" | "require"
default_backend = "auto"  # "auto" | "backup-copy" | "jai" | "zfs"
backup_dir = ""        # Default: adjacent to modified file. Set to centralise backups.
backup_retention = "keep-until-commit"  # "keep-until-commit" | "keep-forever" | "keep-N-days"
intercept_writes = false  # true = auto-backup all writes in an active transaction
auto_commit_on_turn_end = false  # true = commit active transactions when agent turn ends

[transactions.sensitive_paths]
patterns = ["~/.openclaw/**", "/etc/**", "~/.config/**"]

[transactions.zfs]
datasets = []   # List of ZFS datasets to snapshot, auto-detected if empty
ssh_snapshot_before = true  # Take ZFS snapshot before SSHing to known ZFS hosts
```

### Tradeoffs

**Pros:**
- Explicit and composable
- Agent has full control — when to start, commit, rollback
- Works with any claw type as long as NZC is the executor
- Durable across crashes (SQLite)
- Clear API surface for tooling

**Cons:**
- Requires agent to correctly identify when to use it — exactly when agents make mistakes
- Opt-in means it won't be used for things the agent doesn't think are dangerous
- Doesn't help if the agent crashes before calling `transaction_start`

---

## Proposal B: PolyClaw Meta-Command Interface

### The API

PolyClaw exposes meta-commands that operators (human or automated) can invoke:

```
!txn start [label] [--backend auto|backup-copy|jai|zfs]
!txn status
!txn commit [--message "reason"]
!txn rollback [--reason "why"]
!txn list
!txn diff
```

These are delivered as PolyClaw-level messages, not agent tool calls. PolyClaw intercepts them before routing to the underlying claw.

### How PolyClaw Coordinates with the Active Claw

**Problem:** PolyClaw knows `!txn start` was invoked, but how does it know what filesystem operations the claw subsequently performed?

**Options:**

**Option B1: Agent self-reports (lightweight)**
- `!txn start` instructs PolyClaw to inject a system prompt addition into the agent's context: "You are now inside transaction TXN-123. Before modifying any file, call `transaction_file_declare(path)` to register it with the transaction."
- The agent declares files it intends to modify; PolyClaw/NZC backs them up before the write.
- On `!txn rollback`: PolyClaw sends a tool call to NZC to restore all declared files.

**Option B2: PolyClaw intercepts tool calls (strong)**
- PolyClaw wraps every outbound tool call through its routing layer.
- When `write(path, ...)` or `edit(path, ...)` passes through while a transaction is active, PolyClaw triggers a pre-write backup.
- No agent cooperation needed.

Option B2 is architecturally better but requires PolyClaw to understand NZC's tool call format, which couples them. For now, B1 is more realistic.

### How It Works Across Different Claw Types

| Claw | Interception | Rollback |
|---|---|---|
| NZC | Native (backup-copy/jai backend in-process) | Reliable |
| OpenClaw | PolyClaw cannot intercept OpenClaw's file writes | Only if agent declared files via B1 |
| Other | Not supported in v1 | |

**Cross-claw limitation:** For OpenClaw claws, `!txn start` would have to inject a "please declare your file operations" instruction into the agent context — trust-based, not enforced.

### Worked Example

```
[Human to PolyClaw]
!txn start "add PolyClaw channel to Librarian"

[PolyClaw responds]
Transaction started: TXN-abc123 (backup-copy backend)
Claw: NZC / Librarian@10.0.0.20
Note: all file writes will be backed up before modification.
Use !txn commit or !txn rollback when done.

[Human]
Now edit /root/.openclaw/openclaw.json to add the polyclaw channel.

[Agent executes tool calls; PolyClaw intercepts write(openclaw.json, ...)]
[PolyClaw auto-backs up openclaw.json before write proceeds]

[Agent]
Done. Gateway restarted. Health check passed.

[Human]
!txn commit --message "PolyClaw adapter added successfully"

[PolyClaw]
Transaction TXN-abc123 committed.
Files changed: /root/.openclaw/openclaw.json
Backups archived to: ~/.nzc/txn-archives/TXN-abc123/
```

### Composition and Nesting

**Human-initiated nesting is not recommended.** `!txn start` while another is active: PolyClaw prompts "Transaction TXN-abc123 is already active. Nest inside it? [yes/no]"

Nesting semantics mirror Proposal A: inner rollback doesn't affect outer; outer rollback restores everything.

**Cross-SSH:** Not supported. `!txn start` protects the local claw's filesystem. When the agent SSHes away, PolyClaw should emit: "⚠️ Agent is crossing SSH boundary to 10.0.0.40. Remote operations are NOT covered by the active transaction. Consider !txn ssh-guard 10.0.0.40 to take a remote ZFS snapshot first."

### Crash Mid-Transaction

Same SQLite durability as Proposal A, but persisted in PolyClaw's database (which spans multiple claws). On PolyClaw restart:
1. Finds orphaned transactions
2. Routes recovery message to the appropriate claw: "Transaction TXN-abc123 was interrupted. Please resolve: `!txn status TXN-abc123`"

### Configuration Surface

```toml
[polyclaw.transactions]
enabled = true
intercept_writes = true   # Intercept tool calls automatically (requires PolyClaw to understand NZC tool format)
inject_on_start = true    # Inject "you are in a transaction" into agent context
default_backend = "auto"

[polyclaw.transactions.ssh_guard]
auto_snapshot_zfs_hosts = ["10.0.0.70", "10.0.0.40"]  # ZFS snapshot before SSH to these hosts
warn_on_ssh_boundary = true
```

### Tradeoffs

**Pros:**
- Human-in-the-loop: operator explicitly controls when transactions start/end
- Works across claw types (with B1 self-reporting fallback)
- Visible at the conversation level — agent and user both see the transaction state
- Good for production operations where a human is present and watching

**Cons:**
- Requires human to initiate — no automation benefit unless PolyClaw auto-starts on certain triggers
- Cross-claw support is trust-based for non-NZC claws
- PolyClaw must understand NZC tool call format for B2 interception — tight coupling
- Clunkier for routine operations where a human isn't watching

---

## Proposal C: Implicit/Automatic

### The Mechanism

NZC automatically detects "I'm about to write somewhere dangerous" and either:
- Starts a transaction automatically (suggest/require mode)
- Asks the user for confirmation
- Blocks the write until confirmed

No agent action required. The agent just calls `write(path, content)` and NZC decides whether to protect it.

### Heuristics for "Dangerous"

The hard part of Proposal C is defining what's dangerous. Proposal:

**Tier 1 — Always protect (require transaction or backup, no override without explicit config):**
- Any file under `/etc/`
- Any file in `~/.openclaw/` (OpenClaw config and credentials)
- Any file matching `*.json` within config directories
- Any file in `~/.ssh/`
- Any file in `/root/` (if agent is running as root)

**Tier 2 — Protect unless explicitly excluded:**
- Any file in `~/.config/`
- Any file in service config directories (detected via file extension + path pattern)
- Any file outside the NZC workspace (`~/.nzc/workspace/`) and outside designated data directories

**Tier 3 — Safe (no transaction needed):**
- Any file under `~/.nzc/workspace/` (agent's own workspace)
- Any file under `~/.nzc/memory/` (agent memory)
- Any file in `/tmp/`
- Files explicitly allowlisted by the operator

### Detection Implementation

In NZC's `write` and `edit` tool handlers:

```
fn should_auto_protect(path: &Path) -> ProtectionLevel {
    if is_in_safe_dirs(path) { return None }
    if is_in_tier1(path) { return RequireTransaction }
    if is_in_tier2(path) { return SuggestTransaction }
    None
}

fn handle_write(path, content, active_txn) {
    let protection = should_auto_protect(path);
    match (protection, active_txn, config.transactions.mode) {
        (RequireTransaction, None, _) if mode == "require" =>
            error("Write to {} requires an active transaction. Call transaction_start() first.", path),
        (RequireTransaction, None, _) if mode == "suggest" =>
            warn("Write to {} is protected. Auto-starting backup transaction.", path),
            auto_start_transaction(path),
        _ => proceed_with_write(path, content)
    }
}
```

### How the Agent Commits or Rolls Back

**Auto-commit on turn end (optional, configurable):** If `transactions.auto_commit_on_turn_end = true`, any implicitly-started transaction is committed when the agent's turn ends (no more tool calls pending). This is the "fire and forget" mode.

**Auto-rollback on error:** If the agent returns an error response or the gateway detects a health check failure after a write to sensitive paths, NZC can auto-rollback.

**Explicit tool calls for manual control:** Even in implicit mode, the agent can call `transaction_rollback(active_txn)` if it detects a problem. The transaction handle is available via `transaction_list()`.

**Injected context:** When NZC auto-starts a transaction, it injects into the agent's tool call response: `[NOTE: Implicit transaction TXN-xyz started for this write. Call transaction_rollback('TXN-xyz') if you need to undo this change.]`

### Config Option: `transactions.mode`

```toml
[transactions]
mode = "off"         # No protection. Agent operates normally.
                     # For dev/testing or trusted environments.

mode = "suggest"     # Warn on writes to sensitive paths if no transaction active.
                     # Auto-starts a transaction with backup-copy backend.
                     # Proceeds without blocking. Good default for transition period.

mode = "require"     # Block writes to sensitive paths unless inside a transaction.
                     # Agent MUST call transaction_start() before touching protected files.
                     # Right choice for production.
```

Default recommendation: ship with `"suggest"` during initial rollout, migrate to `"require"` once the agent ecosystem reliably uses transactions.

### Worked Example

**Agent in `suggest` mode:**

```
// Agent calls: write("/root/.openclaw/openclaw.json", new_content)

// NZC intercepts:
// Path /root/.openclaw/openclaw.json matches Tier 1 pattern.
// Mode is "suggest". No active transaction.
// Auto-starting transaction TXN-auto-001 with backup-copy backend.
// Backing up /root/.openclaw/openclaw.json → /root/.openclaw/openclaw.json.bak.1711836000

// Write proceeds.
// Response includes: "[Auto-transaction TXN-auto-001 started. Call transaction_rollback('TXN-auto-001') to undo.]"

// Agent calls: exec("openclaw gateway restart")
// Gateway fails to start.

// Agent recognizes failure from health check.
// Agent calls: transaction_rollback("TXN-auto-001")

// NZC restores /root/.openclaw/openclaw.json from backup.
// Gateway restarted with original config.
// Config corruption averted.
```

### Composition

**Implicit transactions don't nest** — if an implicit transaction is already active when the agent makes another protected write, NZC adds the new file to the existing transaction's scope (don't create a new transaction per file). One transaction covers all protected writes in a turn.

**Explicit + implicit:** If the agent calls `transaction_start()` explicitly AND then makes a write that would trigger an implicit transaction, the explicit transaction takes precedence. The write is added to the explicit transaction's scope.

### Cross-SSH

Implicit protection covers local writes only. When agent SSHes to a remote host:
- Implicit protection does NOT follow
- NZC emits: "⚠️ You are executing remote commands on 10.0.0.40. Local transaction TXN-auto-001 does NOT cover remote file changes."
- If `transactions.ssh_guard.auto_snapshot_zfs_hosts` is configured, NZC takes a remote ZFS snapshot before allowing SSH exec.

### Crash Mid-Transaction (Implicit)

Same SQLite durability as Proposal A. Implicit auto-transactions are persisted the same way. On restart: listed as orphaned, human/agent decides to commit or rollback.

### Configuration Surface

```toml
[transactions]
mode = "suggest"
default_backend = "auto"
auto_commit_on_turn_end = false
auto_rollback_on_exec_failure = true  # If exec tool call returns non-zero after a protected write, auto-rollback

[transactions.sensitive_paths]
tier1 = ["/etc/**", "~/.openclaw/**", "~/.ssh/**", "/root/**"]
tier2 = ["~/.config/**", "**/*.service", "**/openclaw.json", "**/*.json"]
safe = ["~/.nzc/workspace/**", "~/.nzc/memory/**", "/tmp/**"]
```

### Tradeoffs

**Pros:**
- No agent cooperation needed — just works
- Catches cases where the agent DIDN'T know it should use a transaction
- `require` mode creates a hard safety boundary
- Simpler agent prompting — agents don't need to learn to call `transaction_start`

**Cons:**
- Heuristics have false positive/negative rates. Overly broad Tier 2 = too many auto-transactions. Too narrow = misses dangerous operations.
- Auto-commit on turn end may commit things the agent would have wanted to roll back.
- Harder to reason about from agent's perspective — when implicit transactions fire, the agent needs to be aware via injected context.
- Configuration is complex (path patterns need tuning per deployment).

---

## Comparison

| Dimension | Proposal A (Explicit NZC) | Proposal B (PolyClaw Meta) | Proposal C (Implicit) |
|---|---|---|---|
| Agent opt-in required | Yes | Human-in-loop | No |
| Works without human | ✅ | ❌ (human initiates) | ✅ |
| Coverage of forgot cases | ❌ | ❌ | ✅ |
| False positive risk | Low | Low | Medium |
| Cross-claw support | NZC only | All claws (limited) | NZC only |
| Crash durability | ✅ SQLite | ✅ SQLite | ✅ SQLite |
| Nesting | Full | Limited | No |
| Complexity (implement) | Medium | High | Medium |
| Operator control | Agent-level | Human-level | Config-level |

---

## Recommendation

**Build Proposal A first. Add Proposal C's detection heuristic as a safety net.**

Specifically:
1. Implement Proposal A's tool-call interface (`transaction_start`, `transaction_commit`, `transaction_rollback`) in NZC.
2. Implement the path-based detection from Proposal C as the `transactions.mode = "suggest"` behavior — when a write hits a sensitive path and no transaction is active, warn/auto-backup.
3. Defer Proposal B (PolyClaw meta-commands) until PolyClaw's adapter layer is built (Session 3). The coordination mechanism depends on PolyClaw architecture that doesn't exist yet.

This gives:
- Agents can be explicit and correct (Proposal A)
- Agents that forget get a safety net (Proposal C's suggest mode)
- No over-engineering before PolyClaw is built

**Do NOT ship `mode = "require"` until agent prompts reliably include transaction instructions.** Blocking agent operations without a clear path to resolution will frustrate users. Ship `"suggest"` first, measure, then graduate to `"require"` for sensitive operations.
