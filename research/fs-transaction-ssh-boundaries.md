# Filesystem Transactions: SSH Boundary Documentation

_Research date: 2026-03-30_
_Context: What transactions can and cannot protect when agent operations cross SSH boundaries_

---

## The Core Problem

When NZC executes a transaction locally, it can:
- Intercept write/edit tool calls
- Back up files before modification
- Restore files on rollback

When NZC executes `exec("ssh root@10.0.0.40 'edit some/config'"`)`, none of that applies:
- The actual write happens inside an SSH session on a remote host
- NZC never sees the individual file operations
- There is no mechanism for local NZC to intercept a write made by a shell on a remote machine
- The backup, if it happens at all, must happen on the remote machine

**This is not a bug to be fixed. It is a fundamental architectural constraint.** The transaction abstraction is a local mechanism. Cross-SSH is a different problem requiring per-host solutions.

---

## What Local Transactions Protect

A local transaction (backup-copy, jai, or ZFS snapshot of a local dataset) covers:

| Operation | Protected? | Notes |
|---|---|---|
| `write("/etc/something")` | ✅ Yes | NZC intercepts, backs up locally |
| `edit("/root/.openclaw/openclaw.json")` | ✅ Yes | NZC intercepts, backs up locally |
| `exec("sed -i ... /local/file")` | ⚠️ Partial | NZC can't easily intercept exec-level writes; must use write/edit tools |
| `exec("ssh host 'edit file'")` | ❌ No | Remote operation, no interception |
| `exec("scp /local/file host:/remote/file")` | ❌ No | Remote side is out of scope |
| `exec("ssh host 'systemctl restart openclaw'")` | N/A | No file write, but service state changed |

**Key caveat on `exec`:** The `exec` tool is opaque. NZC cannot know what files an `exec` call will write. If the agent calls `exec("python3 configure.py")` and that script modifies 5 config files, NZC has no way to intercept those writes. The only safe approach for `exec`-level operations is to:
1. Use ZFS snapshot (covers everything on the filesystem)
2. Use jai (wraps the exec'd process in COW overlay)
3. Use backup copy with agent cooperation (agent explicitly backs up files before calling exec)

---

## Local-Only Mechanisms: Summary

| Mechanism | What It Protects | What It Doesn't Protect |
|---|---|---|
| **Backup copy** | Named files the agent declared or that NZC intercepted via write/edit tool calls | Writes made via `exec`, remote files, files the agent didn't declare |
| **jai/overlayfs** | Any file write made by the jailed process (including via exec'd subprocesses) | Remote host filesystems, files on other mount namespaces |
| **ZFS snapshot (local)** | Entire ZFS dataset — complete filesystem state | Remote hosts, non-ZFS filesystems, files on other datasets |
| **Landlock** | Restricts write surface (but doesn't back up) | Remote hosts; provides no rollback |

---

## Per-Host Strategies for Cross-SSH Operations

### Strategy 1: ZFS Snapshot on Remote Host (Recommended for ZFS Hosts)

Before SSHing to a remote ZFS host for any operation that modifies files:

```
// Agent is about to SSH to 10.0.0.40 (Docker VM, known to have ZFS)
// NZC takes a remote snapshot first

exec("ssh librarian@10.0.0.40 'zfs snapshot tank/appdata@before-op-$(date +%s)'")
// Record snapshot name in active transaction log

exec("ssh librarian@10.0.0.40 'edit /path/to/config'")
// ... do work ...

// On success:
exec("ssh librarian@10.0.0.40 'zfs destroy tank/appdata@before-op-...'")

// On failure:
exec("ssh librarian@10.0.0.40 'zfs rollback tank/appdata@before-op-...'")
```

**Configuration:**
```toml
[transactions.ssh_guard]
auto_snapshot_zfs_hosts = [
    { host = "10.0.0.40", dataset = "tank/appdata" },
    { host = "10.0.0.70",  dataset = "rpool/data" },
]
```

When `auto_snapshot_zfs_hosts` is configured and the agent calls `exec` with an SSH destination matching a listed host, NZC automatically takes the ZFS snapshot before allowing the exec to proceed.

**Tradeoffs:**
- ✅ Truly atomic: snapshot covers the entire dataset, not just declared files
- ✅ Clean rollback: `zfs rollback` is fast and complete
- ✅ Works for opaque exec calls (captures everything the remote process might write)
- ❌ Requires root or delegated ZFS privileges on the remote host
- ❌ `zfs rollback` destroys intermediate snapshots — must coordinate with backup schedule

### Strategy 2: Backup Copy on Remote Host (Universal Fallback)

For hosts without ZFS, or when the agent knows specifically which files will change:

```
// Before SSH operation:
exec("ssh librarian@10.0.0.40 'cp /path/to/config /path/to/config.bak.$(date +%s)'")
// Record backup path in transaction log

// Do work on remote:
exec("ssh librarian@10.0.0.40 '...'")

// On success: optionally clean up backup
// On failure: restore
exec("ssh librarian@10.0.0.40 'cp /path/to/config.bak.TIMESTAMP /path/to/config'")
```

**Tradeoffs:**
- ✅ Universal — works on any host
- ✅ No root required (as long as the user owns the files)
- ⚠️ Only covers declared files, not exec-level surprises
- ⚠️ Backup is on the same host — if the host fails, backup is lost too

### Strategy 3: jai on Remote Host (Future)

If the remote host has jai installed and kernel 6.13+:

```
exec("ssh librarian@10.0.0.40 'jai --storage /tmp/jai-sessions my-agent-op.sh'")
```

After the operation, if it succeeded, extract changes from the upper dir and apply; if not, discard.

**This is too complex for initial implementation.** Remote jai management adds a whole layer of complexity (which jai session? which storage? how does NZC read the remote upper dir?). Defer.

### Strategy 4: No Protection (Current Default)

The current state: agents SSH to remote hosts and make changes with no transaction protection. This is why the 2026-03-30 incident (Librarian config corruption) happened — the agent edited openclaw.json directly without backing it up first.

**This should be the explicit fallback mode, not the default.** Make the lack of protection visible.

---

## Recommended Agent Behavior at SSH Boundaries

### Rule 1: Warn Before Crossing SSH Boundary if Transaction is Active

If the agent calls `exec` with an SSH command while a local transaction is active, NZC should inject a warning:

> ⚠️ **SSH Boundary Warning:** You are crossing to remote host `10.0.0.40`. Your active transaction (TXN-abc123) does NOT cover remote file changes. Remote writes will NOT be rolled back if this transaction rolls back. Consider:
> - `transaction_take_remote_snapshot(txn, "10.0.0.40", "tank/appdata")` to protect the remote host with a ZFS snapshot
> - Manually backing up remote files before modifying them
> - Acknowledging that remote changes are intentionally unprotected

### Rule 2: Auto-Snapshot for Configured ZFS Hosts

If `transactions.ssh_guard.auto_snapshot_zfs_hosts` lists the target host:
- Automatically take the ZFS snapshot before allowing the SSH exec
- Add the snapshot reference to the active transaction log
- On transaction rollback: auto-rollback the remote snapshot too
- On transaction commit: auto-destroy the remote snapshot

This is the best possible protection for ZFS hosts and requires no agent cooperation.

### Rule 3: Refuse SSH in `require` Mode Unless Protected

If `transactions.mode = "require"` AND `transactions.ssh_require_protection = true`:

Refuse `exec(ssh_command)` unless either:
- The target host is in `auto_snapshot_zfs_hosts` (auto-protected), OR
- The agent has explicitly called `transaction_declare_ssh_unprotected(host)` to acknowledge the lack of protection

This is strict and may be too aggressive for initial rollout. Flag as a future option.

### Rule 4: Emit Tool Documentation

In NZC's tool call documentation (injected into agent context or available via tool introspection):

```
write(path, content) — Writes content to a file.
  TRANSACTION: Backs up path before writing if a transaction is active.
  SENSITIVE: If path matches sensitive patterns, auto-transaction may start.
  ⚠️  SSH: Does not apply to remote files. Use exec(ssh ...) for remote writes.

exec(command) — Executes a shell command.
  ⚠️  SSH BOUNDARY: If command contains ssh/scp/rsync to a remote host:
      - Local transactions do NOT cover remote file changes.
      - Configure transactions.ssh_guard for automatic remote ZFS snapshots.
      - Manually back up remote files if no ssh_guard is configured.
```

---

## Summary: The Transaction Model and SSH

```
LOCAL MACHINE (NZC running here)
┌─────────────────────────────────────────────────────┐
│  Transaction scope:                                  │
│  ✅ write(local_file)      → intercepted, backed up  │
│  ✅ edit(local_file)       → intercepted, backed up  │
│  ⚠️  exec(local_script)    → if script writes files, │
│                              NOT intercepted unless  │
│                              ZFS snapshot or jai used│
│                                                      │
│  exec("ssh remote ...")   → crosses SSH boundary ↓  │
└────────────────────────────┬────────────────────────┘
                             │ SSH
REMOTE MACHINE (10.0.0.40, .52, etc.)
┌────────────────────────────▼────────────────────────┐
│  ❌ Local transaction provides ZERO coverage here    │
│                                                      │
│  Protection options:                                 │
│  🥇 ZFS snapshot (if ZFS available) — auto via      │
│     transactions.ssh_guard config                   │
│  🥈 Remote backup copy — manual or agent-coordinated│
│  🥉 None — document and accept the risk             │
└─────────────────────────────────────────────────────┘
```

---

## How This Should Appear in Tool Documentation / System Prompts

**Short version for agent system prompt:**

```
## Filesystem Safety
- Wrap dangerous file operations in `transaction_start()` / `transaction_commit()` / `transaction_rollback()`
- Local transactions back up files before write/edit tool calls
- LOCAL ONLY: transactions do NOT cover SSH operations on remote hosts
- For remote operations: take ZFS snapshots before SSH (auto-configured for known ZFS hosts)
- When in doubt: back up first, modify second, verify third
```

**Longer version for NZC tool introspection manifest:**

```json
{
  "tool": "exec",
  "safety_notes": [
    "If command writes files on the LOCAL host, prefer write/edit tools (transaction-interceptable)",
    "SSH operations cross a transaction boundary — remote files are NOT protected by local transactions",
    "ZFS hosts in transactions.ssh_guard receive automatic snapshots before exec(ssh ...) is called",
    "Unprotected SSH ops to sensitive hosts should be preceded by manual remote backup-copy"
  ]
}
```

---

## Open Questions for Brian

1. **Which remote hosts have ZFS?** Need a definitive list (10.0.0.70=Proxmox=yes, 10.0.0.40=Docker VM=depends on setup) to configure `auto_snapshot_zfs_hosts`. Brian should confirm which datasets to snapshot.

2. **Privilege model for remote ZFS:** Does the `librarian` SSH key have `zfs` delegation? Currently root is used for destructive ops. Should we create a `zfs-snapshot` delegation for the librarian user?

3. **Cross-host transaction atomicity:** Is there ever a scenario where we need both local AND remote changes to atomically commit/rollback together? If yes, this requires a two-phase commit protocol — much more complex, probably not worth it for v1. Worth discussing the use cases.
