# Filesystem Transaction Alternatives Survey

_Research date: 2026-03-30_
_Context: Alternatives to jai for providing "filesystem transaction" semantics in NZC/ZeroClawed_

---

## The Goal

When an agent makes dangerous filesystem changes (editing OpenClaw config, running an installer step, migrating credentials), we want:

1. **Rollback**: if something goes wrong, get back to where we started
2. **Atomicity** (nice to have): either all changes commit or none
3. **Visibility**: agent and user can see what changed
4. **Cross-SSH** (partially): ideally some protection even on remote hosts

---

## Approach 1: Backup Copy (Simplest)

**Mechanism:** Before every write to a sensitive file, copy it: `cp file file.bak.$(date +%s)`

**Implementation complexity:** Trivial. Maybe 20 lines of Rust.

**Cost:** One `cp` syscall + disk write per file. For config files (small), this is microseconds. For large files, milliseconds. No startup cost.

**Reliability:** 
- ✅ Extremely reliable — just files
- ✅ No kernel version requirements
- ✅ Works anywhere (local, SSH'd, NFS, any filesystem)
- ⚠️ NOT atomic: if agent writes fileA.bak, then fileB, then crashes — you have fileB written but no fileB.bak. Partial rollback is possible but requires bookkeeping.
- ⚠️ Rollback is manual unless you build tooling around it
- ⚠️ Backup files accumulate unless cleaned up

**Failure modes:**
- Process crashes mid-write: original file may be partially written. Backup is intact, so rollback is possible IF you know which files to restore.
- Disk full during backup copy: the write fails (safe — original untouched), but the operation is blocked.
- Multiple files in a transaction: rollback requires knowing the full set of files changed. If the agent doesn't track this list, you can't reliably roll back.
- Human accidentally deletes backup files.

**Cross-SSH behavior:** Works perfectly across SSH — just copy files before writing them on the remote host. Agent SSHs in, runs `cp file file.bak` first, proceeds. If something goes wrong, agent SSHs back in and runs `cp file.bak file`.

**User experience:** Simple to understand. Backup files are visible, manageable, and standard. Mildly clutters directories if not cleaned up.

**Recommendation:** This is the right **initial backend**. Ship it first, upgrade later.

---

## Approach 2: jai / overlayfs COW

**Mechanism:** Wrap the operation in a kernel overlayfs overlay. All writes go to an upper dir. On success: walk upper dir and apply to real filesystem. On rollback: discard upper dir.

**Implementation complexity:** Significant. Requires: jai binary (or implementing overlayfs setup natively), a commit step (walk upper dir, handle whiteouts, apply atomically), cleanup logic.

**Cost:** 30–100ms startup per overlay creation. Fast enough for per-operation-group, too slow for per-individual-write.

**Reliability:**
- ✅ Strong isolation — jailed process literally cannot corrupt real files
- ✅ Rollback is free: just discard the upper dir
- ✅ Atomic within a session (from the jailed process's perspective)
- ⚠️ Commit is NOT atomic (replay of upper dir is a series of individual renames/copies)
- ⚠️ Requires kernel 6.13+ (jai uses new mount API)
- ⚠️ Requires setuid or unprivileged user namespaces
- ⚠️ overlayfs is "flaky" per jai's own documentation — xattr sync issues

**Failure modes:**
- Commit step crashes halfway: partial commit. Worse than backup copy because there's no per-file backup.
- Kernel overlayfs bug: rare but documented — jai's man page warns about attribute sync issues.
- Operation crosses directories on different filesystems: overlayfs requires same underlying filesystem for upper/lower dirs in some configurations.

**Cross-SSH behavior:** **Completely irrelevant across SSH boundaries.** The overlay protects the local machine only. When the agent SSHs to 10.0.0.40 and edits files there, jai provides zero protection.

**User experience:** From the user's perspective, changes either appear or don't. The upper dir (`~/.jai/<sandbox>.changes/`) is inspectable but cryptic (whiteouts, overlayfs metadata).

**Recommendation:** Excellent for wrapping an entire agent session (all of `jai claude` semantics). Less suitable as a per-operation transaction backend because the commit step complexity is high and the kernel requirement is limiting.

---

## Approach 3: ZFS Snapshot

**Mechanism:** Before dangerous operations, take a ZFS snapshot. If rollback needed, `zfs rollback`. If success, destroy snapshot.

**Implementation complexity:** Moderate. Requires detecting if host has ZFS, finding the right dataset, calling `zfs snapshot`/`zfs rollback`. Via SSH, add the SSH hop.

**Cost:** ZFS snapshot is nearly instantaneous (metadata-only, COW under the hood). Rolling back is fast if no other snapshots exist between now and then.

**Reliability:**
- ✅ Truly atomic — snapshot is instantaneous
- ✅ Full filesystem state captured, including files the agent didn't know about
- ✅ Rollback is clean and complete
- ✅ Remote-capable: `ssh root@host zfs snapshot tank/dataset@before-op`
- ❌ **Requires ZFS.** Not universally available. Proxmox/TrueNAS hosts: yes. Random VPS, Docker VM with ext4: no.
- ❌ Requires root or delegated ZFS privileges to create/rollback snapshots
- ⚠️ `zfs rollback` destroys all snapshots newer than the target — must be careful with snapshot ordering
- ⚠️ Dataset granularity: one snapshot covers the entire dataset, not just the files you care about

**Failure modes:**
- Host doesn't have ZFS: approach unavailable, need fallback.
- Rollback destroys newer snapshots (Proxmox backup snapshots, etc.): operator must be careful.
- Dataset is wrong (e.g., the config file is on a different dataset than assumed): rollback covers wrong scope.

**Cross-SSH behavior:** **This is the BEST option for remote hosts.** `ssh root@10.0.0.40 "zfs snapshot tank/data@jai-op-$(date +%s)"` is safe, fast, and reliable. The agent can take a remote snapshot before SSHing in to do work, then roll back or destroy the snapshot based on outcome.

**User experience:** Transparent. Users familiar with ZFS understand snapshots. `zfs list -t snapshot` shows what's pending. Clean destroy when done.

**Recommendation:** The right choice when the remote host has ZFS (Proxmox VMs, NAS boxes). Should be the `transactions.backend = "zfs"` option in config. Not viable as a universal default.

---

## Approach 4: Git Staging Area

**Mechanism:** Treat the config directory as a git repo. Before changes: `git add -A && git stash` (or just track it). After changes: `git diff` shows what changed. On rollback: `git checkout .` or `git stash pop`.

**Implementation complexity:** Moderate. Git must be present. Need to initialize repos, manage commits/stashes, handle `.gitignore`, handle binary files.

**Cost:** `git add` + `git commit` is fast for small config dirs (100ms or less). `git diff` output is readable.

**Reliability:**
- ✅ Every change is tracked with a commit message (audit trail)
- ✅ Rollback to any prior state is easy
- ✅ Human-readable diffs
- ✅ Works on any filesystem with git
- ⚠️ Does NOT protect against writes made BEFORE git was initialized
- ⚠️ Only covers the tracked directory — not system-wide
- ⚠️ Binary files work but diffs are ugly
- ⚠️ Credential files in the repo are a security concern (gitignore them, but then they're not protected)
- ❌ NOT atomic: `git checkout .` restores tracked files but doesn't reverse new file creation (untracked files remain)

**Failure modes:**
- Process crashes mid-write: git has the pre-write state committed, so rollback works perfectly
- Large files (binary configs, databases): git objects bloat
- `.git` directory corruption: rare but possible on power loss

**Cross-SSH behavior:** Works over SSH if you run git commands on the remote host. But you need git installed and a repo initialized there, which is a per-host setup burden.

**User experience:** Power users love git-based auditing. Less sophisticated users find it confusing. The audit trail is genuinely valuable — every change to `openclaw.json` has a commit message and diff.

**Recommendation:** Excellent for config directories that benefit from long-term audit trails (`~/.openclaw/`, NZC config dir). Consider using git as an audit layer independently of the transaction backend, not as the transaction backend itself. `git diff` output after a proposed change is a useful UX for showing the user what will change.

---

## Approach 5: tmpfs Staging

**Mechanism:** Write new file version to tmpfs (RAM), validate it, then `rename()` (atomic) to the real location.

**Implementation complexity:** Simple. ~30 lines.

**Cost:** Negligible. RAM write + kernel rename syscall.

**Reliability:**
- ✅ Single-file commits are **atomic** — `rename()` is atomic on POSIX filesystems
- ✅ Works everywhere
- ✅ No kernel version requirements
- ⚠️ Only atomic for a single file. Multi-file transactions are NOT atomic.
- ⚠️ Requires the target and tmpfs to be on the same filesystem for atomic rename (or same mount — use `O_TMPFILE` + `linkat` for cross-filesystem)
- ❌ No rollback for multi-file transactions without additional bookkeeping

**Failure modes:**
- Single file: essentially impossible to corrupt. Rename is atomic.
- Multi-file: if you write fileA and fileB, and the process crashes between the two renames, fileA is new and fileB is old. Inconsistent state.

**Cross-SSH behavior:** Usable via SSH — write to `/tmp/` on remote, validate, rename. The rename is atomic on the remote host.

**User experience:** Invisible. No backup files, no special directories.

**Recommendation:** The right approach for **individual file writes** (single-config-file edits). Not suitable as a general transaction backend. NZC should use tmpfs staging for individual file writes as a base safety measure, independently of the transaction system.

---

## Approach 6: Landlock

**Mechanism:** Landlock is a Linux kernel access control mechanism (LSM). It restricts what filesystems paths a process can read/write. The `sandbox-landlock` feature flag is already in NZC's Cargo.toml.

**This is NOT a transaction mechanism.** Landlock provides **access control** (prevent writes to paths you shouldn't touch) but provides **no rollback** and **no commit**. It's complementary.

**What it actually does:**
- Before a risky operation: restrict the process to only read/write the specific files it should touch
- If the agent attempts to write to `/etc/something` when it shouldn't: blocked at the kernel level
- On success: no benefit beyond the restriction itself

**Relationship to transactions:**
Landlock + backup copy = a useful combination:
- Landlock prevents unintended writes (you can only touch the files you declared)
- Backup copy ensures you can roll back the files you declared

**Cross-SSH behavior:** Landlock restricts the local process. It provides no protection once the process SSHes to a remote host.

**Kernel version requirement:** Landlock requires Linux 5.13+ (ABI v1), 5.19+ (v2), 6.1+ (v3). Much more widely available than jai's 6.13 requirement.

**User experience:** Invisible. Either it works or the operation is blocked with an error.

**Recommendation:** Use Landlock as a **defense-in-depth layer** alongside the transaction backend, not as a replacement for it. Landlock says "you may only touch these files"; backup copy says "if you do touch them, here's how to undo it."

---

## Comparative Matrix

| Approach | Rollback | Atomic | Cross-SSH | Kernel Req | Complexity | Best For |
|---|---|---|---|---|---|---|
| Backup copy | ✅ Manual | ❌ Multi-file | ✅ Yes | None | Trivial | Universal default |
| jai/overlayfs | ✅ Free | ✅ Isolation | ❌ No | 6.13+ | High | Full session wrapping |
| ZFS snapshot | ✅ Clean | ✅ Yes | ✅ Best | ZFS needed | Moderate | ZFS hosts, remote ops |
| git staging | ✅ Any rev | ❌ | ⚠️ Manual | None | Moderate | Audit trail, config dirs |
| tmpfs staging | ✅ (single) | ✅ Single file | ✅ | None | Simple | Individual file writes |
| Landlock | ❌ None | N/A | ❌ No | 5.13+ | Low | Access control only |

---

## Recommendation

**Don't pick one. Layer them:**

1. **Baseline (all deployments):** Backup copy for rollback + tmpfs staging for atomic single-file writes
2. **ZFS hosts:** ZFS snapshot before any operation touching a ZFS dataset, especially remote
3. **Full session isolation (optional):** jai if kernel 6.13+ is available and agent session should be completely sandboxed
4. **Defense in depth:** Landlock to restrict write surface to declared files
5. **Audit trail:** git commits for long-lived config directories

The transaction API (see `fs-transaction-proposals.md`) abstracts over the backend, so the right combination can be selected per deployment.
