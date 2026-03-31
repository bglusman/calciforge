# jai Integration Levels for NZC / PolyClaw

_Research date: 2026-03-30_
_Context: Three integration levels given that NZC already has `sandbox-bubblewrap` and `sandbox-landlock` feature flags_

---

## Framing

NZC already has infrastructure for sandboxing (bubblewrap, landlock). The question is: how deeply should the transaction system be integrated with jai/overlayfs, and at what cost?

The three levels represent increasing integration complexity and decreasing external dependencies.

---

## Level 0: No jai — Backup Copy Backend

### What It Is

The transaction system ships only with the backup-copy backend. No jai dependency whatsoever.

**When a transaction starts:**
```
transaction_start("edit openclaw config", backend="backup-copy")
→ NZC registers TxnHandle in SQLite
```

**When a protected write occurs:**
```
write("/root/.openclaw/openclaw.json", new_content)
→ NZC copies /root/.openclaw/openclaw.json → /root/.openclaw/openclaw.json.bak.<txn-id>.<timestamp>
→ NZC tracks the backup path in the transaction record
→ Write proceeds
```

**On rollback:**
```
transaction_rollback(txn)
→ For each (original_path, backup_path) in transaction record:
    rename(backup_path, original_path)
→ Transaction marked rolled_back in SQLite
```

**On commit:**
```
transaction_commit(txn)
→ Transaction marked committed
→ Backups optionally deleted or archived (per config)
```

### Installer Behavior

No configuration offered. Backup-copy is always available. The installer does:
- Creates `~/.nzc/backups/` directory (or configures an alternative via `transactions.backup_dir`)
- Sets `transactions.mode = "suggest"` in default config

### Agent Experience

- `transaction_start` and `transaction_commit`/`transaction_rollback` work as documented
- `transaction_diff` shows files that were backed up and have changed
- `transaction_list` shows active transactions
- No special setup required

### User Experience

- Backup files appear adjacent to originals (or in centralized backup dir)
- On rollback: originals are restored, backup files deleted
- Audit trail: SQLite `transactions` table shows history

### Limitations

- Backup copy only intercepts tool-level writes (write/edit tool calls). Writes made by arbitrary `exec` commands (running scripts, config tools, etc.) are NOT intercepted.
- Multi-file atomicity is not guaranteed: if crash occurs mid-rollback, some files restored and some not.
- File backups are on the same filesystem — disk failure loses both original and backup.

### Recommendation

**This is the implementation target for Session 3 (PolyClaw adapter installation).** It's already captured in the safety requirements: "backup config locally before any modification." Level 0 just formalizes and automates what we already planned to do manually.

Build this first. It's three days of work, not three weeks.

---

## Level 1: jai Optional — Runtime Detection, Fallback to Backup Copy

### What It Is

NZC checks at startup (or at `transaction_start` time) whether a `jai` binary is on PATH. If yes, uses jai as the transaction backend. If not, falls back to backup copy.

**Configuration:**
```toml
[transactions]
default_backend = "auto"   # "auto" | "jai" | "backup-copy" | "zfs"
```

With `"auto"`:
- Check for `jai` on PATH
- Check kernel version ≥ 6.13 (jai requirement)
- Check that unprivileged user namespaces are enabled or jai is setuid
- If all checks pass: use jai
- Otherwise: fall back to backup-copy with a log message

**When a transaction starts with jai backend:**
```
transaction_start("dangerous operation", backend="jai")
→ NZC invokes: jai --storage /tmp/nzc-txn/<txn-id>/ --sandbox-name <txn-id> -- <noop-process>
   Wait: jai creates the overlay and starts the sandbox
   NZC then sets up a mount namespace fork (or uses jai's run hook) so the agent's subsequent
   file writes go through the jai overlay
→ On commit: walk /tmp/nzc-txn/<txn-id>/<txn-id>.changes/, replay to real filesystem
→ On rollback: jai -u cleans up, changes discarded
```

**The integration challenge:** jai is designed to be the outer process that wraps another process. NZC is already running. Making jai protect NZC's file writes without wrapping NZC itself requires either:

- **Option L1a:** NZC forks a subprocess for the "transaction body" and runs it inside jai. This is a significant restructuring.
- **Option L1b:** NZC reuses jai's storage/upper-dir concept without actually invoking jai: detect if jai is present and its version, then use the overlayfs setup approach ourselves (effectively reimplementing part of jai's setup in Rust). This is Level 2.
- **Option L1c:** Use jai only for `exec` operations (exec the command inside jai), not for write/edit tool calls. write/edit still use backup copy. `exec("jai -- cmd")` runs cmd inside jai overlay. After exec completes, commit or discard the jai upper dir.

**Option L1c is the pragmatic choice.** It means:
- write/edit tool calls → backup copy (Level 0)
- exec tool calls on dangerous commands → jai wraps the subprocess

### Installer Behavior

At install time, NZC installer:
1. Detects kernel version
2. If kernel ≥ 6.13: offers to check for/install jai
3. If jai found: sets `transactions.default_backend = "auto"` in config
4. If jai not found: sets `transactions.default_backend = "backup-copy"` in config
5. Presents: "jai was found on your system. For stronger transaction protection on exec operations, NZC can use jai as a backend. This requires kernel 6.13+ and jai setuid root. Enable? [yes/no]"

### Agent Experience

For write/edit tool calls: same as Level 0.

For exec tool calls (with jai):
```
// With jai backend active, when agent calls:
exec("python3 install-adapter.py")

// NZC wraps it:
exec("jai --storage /tmp/nzc-txn/TXN-123/ --sandbox-name TXN-123 -- python3 install-adapter.py")

// Changes from install-adapter.py land in overlayfs upper dir
// On commit: NZC replays upper dir to real filesystem
// On rollback: upper dir discarded
```

### User Experience

Mostly invisible. If jai is available, exec-level operations inside transactions are more fully protected. Users see:
- `transaction_diff` shows more complete picture (all writes the subprocess made, not just declared files)
- On rollback: even undeclared subprocess writes are reversed

**Startup cost warning:** jai adds ~50–100ms to `exec` tool calls inside transactions. Fine for config installer operations; noticeable for frequently-called execs.

### Tradeoffs

**vs Level 0:**
- ✅ Exec-level writes are protected (not just write/edit tool calls)
- ✅ Better for complex operations that use scripts/installers
- ❌ jai dependency: kernel 6.13+, setuid root or user namespaces
- ❌ Commit step (replaying overlayfs upper dir) adds complexity and a new failure mode
- ❌ jai is GPL v3 — can't link it, subprocess-only

**Recommendation for Level 1:** Implement L1c (jai wrapping exec calls only). This gives the biggest benefit (protecting opaque exec operations) with the least architectural change. Write/edit tool calls continue using backup copy.

---

## Level 2: Native Overlayfs in NZC — No External jai Binary

### What It Is

NZC implements overlayfs wrapping natively using the existing `sandbox-bubblewrap` infrastructure in `Cargo.toml`. No external `jai` binary needed. NZC itself manages:
- Creating user namespaces (`CLONE_NEWUSER`)
- Creating mount namespaces (`CLONE_NEWNS`)
- Setting up overlayfs via `fsopen`/`fsconfig`/`fsmount`
- Walking the upper dir on commit
- Whiteout handling on rollback

This is essentially reimplementing the core of jai's `make_home_overlay()` in Rust.

### Relationship to `sandbox-bubblewrap`

The `sandbox-bubblewrap` feature flag in NZC's Cargo.toml currently gates code that uses the `bwrap` (bubblewrap) binary as an executor. Bubblewrap handles user + mount namespace creation.

For Level 2, we'd either:
- **Extend bubblewrap integration:** Configure bwrap to use overlayfs as its filesystem layer (bwrap supports `--overlay` since v0.8.0). This is the lower-effort path.
- **Pure Rust overlayfs:** Use `nix` crate syscall wrappers to call `fsopen`/`fsconfig`/`fsmount` directly without bwrap. This is how jai works, in C++. Requires kernel 6.13+ for the new-style mount API.

**Bubblewrap overlay path** (`bwrap --overlay lower upper work mountpoint`):
```
bwrap \
  --overlay / /tmp/nzc-upper-<txn> /tmp/nzc-work-<txn> / \
  --bind /current/cwd /current/cwd \
  -- command args
```
This mounts an overlay over the entire root FS (lower=/, upper=tmp dir), then runs the command. All writes go to the upper dir. On commit, replay upper dir to real FS.

Bubblewrap requires **Linux 5.0+** (much better than jai's 6.13+ requirement) and **does NOT require setuid** on modern kernels with unprivileged user namespaces.

### Installer Behavior

Level 2 is a compile-time feature flag (`--features sandbox-bubblewrap` or similar). The installer:
- Does NOT offer to install jai
- Does NOT configure `transactions.backend = "jai"`
- Sets `transactions.default_backend = "auto"` which resolves to:
  - overlayfs (via bwrap) if user namespaces are available
  - backup-copy otherwise

### Agent Experience

Same API surface as Level 0 and 1. From the agent's perspective, the transaction starts, writes are intercepted, commit/rollback works. The backend is invisible.

The difference is coverage: Level 2 can wrap entire `exec` sessions in COW overlay, catching all subprocess writes.

### User Experience

Fully invisible. No jai binary to install. Works on any kernel 5.0+ system with user namespaces (which is virtually every modern Linux).

### Commit Step Complexity

The commit step (replaying overlayfs upper dir) is non-trivial regardless of whether the overlay was created by jai or by NZC natively. The logic is the same:

1. Walk `upper_dir/` recursively
2. For each entry:
   - Regular file → copy/rename to real path
   - Whiteout char device (0:0) → delete real file
   - Whiteout xattr (`trusted.overlay.whiteout`) → delete real file
   - Directory → ensure real dir exists (merge, don't replace)
   - Symlink → recreate symlink
3. Handle permissions/ownership
4. Delete upper dir on success

This is ~300–500 lines of Rust. It's the same work whether it's for jai (L1) or native (L2). The only question is whether you also have to write the overlayfs setup code or can reuse bwrap.

### Tradeoffs

**vs Level 1:**
- ✅ No jai binary dependency — works anywhere NZC is installed
- ✅ Better kernel version support (5.0+ via bwrap vs 6.13+ for jai)
- ✅ No GPL v3 concern
- ✅ Tighter integration — NZC can interpose on the overlay lifecycle directly
- ❌ More code to write (overlayfs setup in Rust + commit logic)
- ❌ Bubblewrap binary is still a runtime dependency (but it's widely available and small)

**Recommendation for Level 2:** This is the right long-term architecture. The native overlayfs approach via bwrap is better than jai dependency: lower kernel requirement, no GPL concern, no external binary to find/install.

---

## Summary and Recommendation

| | Level 0 | Level 1 | Level 2 |
|---|---|---|---|
| Backend | Backup copy only | backup-copy + optional jai | backup-copy + native overlayfs |
| External deps | None | jai binary (GPL v3) | bwrap binary (LGPL) |
| Kernel req | None | 6.13+ for jai | 5.0+ via bwrap |
| Exec coverage | ❌ (only write/edit) | ✅ (jai wraps exec) | ✅ (bwrap wraps exec) |
| Commit step needed | No | Yes (upper dir replay) | Yes (upper dir replay) |
| Implementation effort | ~1 week | ~2 weeks + jai debugging | ~3 weeks |
| Right for | Session 3 now | 6-12 months out | Long-term target |

### Implementation Path

**Now (Session 3 + immediate after):** Implement Level 0. Ship backup-copy backend with the full transaction API. This is what the PolyClaw adapter installer needs and it's the correct "safe default that works everywhere."

**Phase 2 (3–6 months):** Implement Level 2 (native overlayfs via bwrap extension OR pure-Rust fsmount). This covers exec-level writes and removes the "only write/edit tool calls are intercepted" limitation. Do NOT go through Level 1 first — there's no point adding a jai subprocess dependency when the bwrap path gets us the same capability with better portability.

**Level 1 (jai subprocess):** Only worth implementing if someone specifically requests jai integration for environments already using jai for their agent sessions. The Level 2 bwrap path is strictly better for NZC's purposes.

### What the Installer Should Offer

**Initial installer (Level 0):**
```
Filesystem safety for agent operations:
  ✅ Backup copy will be used for protected file writes.
  Backup directory: ~/.nzc/txn-backups/ [configure]
  Transaction mode: suggest [off / suggest / require]
```

**Future installer (Level 2):**
```
Filesystem safety for agent operations:
  ✅ Overlayfs available (kernel 5.0+, user namespaces enabled)
  This provides stronger protection for exec operations.
  
  Backend: auto [overlayfs-bwrap / backup-copy]
  Transaction mode: suggest [off / suggest / require]
```
