# Filesystem Transaction System: Summary and Recommendations

_Research date: 2026-03-30_
_Author: Librarian (subagent research task)_
_References: jai-analysis.md, fs-transaction-alternatives.md, fs-transaction-proposals.md, fs-transaction-ssh-boundaries.md, jai-integration-levels.md_

---

## What We're Building and Why

The 2026-03-30 incident: Librarian tried to add a PolyClaw channel adapter to `openclaw.json` manually, wrote invalid config, the gateway crashed, and Custodian couldn't recover it. Manual intervention required.

The core problem: **agents make dangerous file writes with no ability to roll back.** The solution: a transaction system that backs up files before modification and restores them if something goes wrong.

This is Workstream 4 from the vault integration plan. It's a research/design task for now; implementation goes into Session 3 (PolyClaw adapter) and beyond.

---

## Interface Recommendation: Proposal A + Proposal C Safety Net

**Primary interface: Proposal A (Explicit NZC Tool-Call Interface)**

```
txn = transaction_start(label, backend?)
  ... do dangerous work ...
transaction_commit(txn)   // or transaction_rollback(txn)
```

**Why A wins over B and C:**
- **vs Proposal B (PolyClaw meta-commands):** PolyClaw doesn't exist yet as a working adapter layer. We need safety now, for Session 3. B requires PolyClaw to intercept tool calls or inject context — complex to build and test before the adapter is done. Defer B until Session 3 is shipped.
- **vs Proposal C (fully implicit):** Implicit transactions are powerful but the heuristics are hard to get right. False positives (auto-backing up files that don't need it) add noise. False negatives (missing a dangerous operation) give false confidence. More importantly, implicit behavior is harder to reason about — the agent needs injected context to know what happened, which adds token overhead.

**Safety net: Proposal C's detection layer**

Ship `transactions.mode = "suggest"` as a second defense layer. When any write/edit call hits a sensitive path pattern with no active transaction, NZC:
1. Logs a warning
2. Auto-starts a backup-copy transaction for that single file
3. Injects a note into the tool response: "[Auto-backed up before write. Call transaction_rollback('TXN-...' to undo.]"

This catches the cases where the agent forgot to call `transaction_start`. It doesn't require the agent to know about transactions at all — but it's also honest about what it is: a fallback, not the primary mechanism.

**Do NOT ship `mode = "require"` until agent prompts reliably include transaction instructions.** Start with `"suggest"`, measure how often agents use transactions explicitly vs rely on auto-backup, then graduate to `"require"` for high-risk operations.

---

## Backend Recommendation: Start with Backup Copy, Upgrade to Overlayfs Later

### For Session 3 (now): Level 0 — Backup Copy

The backup-copy backend is:
- Trivial to implement (copy file before write, restore on rollback)
- Universal (no kernel version requirements, no external dependencies)
- Reliable (standard filesystem operations)
- Already what we planned to do manually ("backup first" is in the safety requirements)

It just needs to be automated, tracked in SQLite, and exposed via the `transaction_start` API.

**This is the only thing that needs to be shipped for Session 3.** The PolyClaw adapter installer needs to:
1. Take a backup of `openclaw.json` before editing
2. Edit it
3. Verify the gateway comes up
4. If yes: commit (keep or archive backup)
5. If no: rollback (restore backup, restart with original config)

That's Level 0 with explicit Proposal A calls. Ship it.

### For Later: Level 2 — Native Overlayfs via Bubblewrap

Once the basic transaction system is working and agent prompts are updated, the upgrade path is Level 2: native overlayfs wrapping for `exec` calls. This covers subprocess writes that the backup-copy backend can't intercept (e.g., an install script that writes 5 config files).

Use bwrap (`sandbox-bubblewrap` feature already in Cargo.toml) to create overlayfs overlays around exec calls inside transactions. No jai binary dependency needed.

**Do not implement Level 1 (jai subprocess) at all.** Level 2 via bwrap is strictly better: lower kernel requirement (5.0+ vs 6.13+), no GPL v3 concern, tighter integration. Level 1 is a dead end.

---

## What Should Go Into Session 3 (PolyClaw Adapter)

Session 3 is the PolyClaw adapter installation — editing live OpenClaw config. This is the highest-risk operation in the current sprint.

**Minimum for Session 3:**

1. **`transaction_start` / `transaction_commit` / `transaction_rollback` tool calls** (Proposal A) implemented in NZC with backup-copy backend.

2. **`transactions.mode = "suggest"` as default config** — auto-backs up writes to `~/.openclaw/**` and other sensitive paths even if the agent didn't call `transaction_start`.

3. **`transaction_diff` tool** — shows the agent what changed before committing (valuable for the "show diff, require confirmation" step in the adapter flow).

4. **SQLite persistence of transaction state** — crash recovery for orphaned transactions.

5. **The PolyClaw adapter installer uses `transaction_start` explicitly** — don't rely on the auto-backup safety net for the most dangerous operation. Be explicit.

**Defer to after Session 3:**

- Proposal B (PolyClaw meta-commands) — needs PolyClaw adapter architecture first
- Level 2 overlayfs backend — adds complexity, not needed for config-file editing
- ZFS remote snapshot auto-guard — useful, but not blocking Session 3
- `transactions.mode = "require"` — wait until agent prompts are updated
- Transaction nesting — useful but not needed for v1

---

## SSH Boundaries: What to Tell Brian

The core constraint: **local transactions do not protect remote hosts.** When the PolyClaw adapter installer SSHes to Librarian at 10.0.0.20 to edit `openclaw.json`, the transaction is running on the machine where NZC is running (the PolyClaw host). If NZC is on a different host from Librarian, the transaction doesn't cover the edit.

**For the immediate use case (Session 3):**
The adapter installer runs on the same host as the target OpenClaw instance. Transaction is local, protection is full. No SSH boundary issue for Session 3.

**For future cross-host operations:**
Configure `transactions.ssh_guard.auto_snapshot_zfs_hosts` to include Proxmox (.52) and the Docker VM (.127). This auto-takes ZFS snapshots before SSH operations on those hosts, providing the best available protection.

---

## Blockers and Open Questions for Brian

### Immediate (Session 3)

1. **Which machine runs the PolyClaw adapter installer?** If it's a separate NZC instance SSHing to Librarian, then the transaction protection is for local files only (see SSH boundaries). If it's running on the same machine as OpenClaw (Librarian's VM at 10.0.0.20), then it's fully local and the backup-copy backend works perfectly. This affects whether we need the SSH guard for Session 3.

2. **NZC's current transaction tooling status:** Does NZC already have any `transaction_*` tool stubs, or is this a from-scratch implementation? Need to know what's already in the codebase before estimating Session 3 effort.

### Near-Term (Phase 2)

3. **Which remote hosts have ZFS and with which datasets?** For the `auto_snapshot_zfs_hosts` config. Proxmox (.52) definitely has ZFS. What about the Docker VM (.127)? What datasets are relevant for OpenClaw config?

4. **`librarian` SSH key ZFS delegation:** Currently root SSH is used for destructive ops. Should we delegate `zfs snapshot/rollback` to the librarian user so the transaction system can operate without root? This is a security improvement (avoid root for snapshots) but requires a Proxmox/ZFS config change.

5. **Bubblewrap availability:** Is `bwrap` installed on NZC's target platforms? It's in most Linux package repos but worth confirming before planning Level 2 implementation.

### Design Decisions That Don't Need Immediate Answers

- Whether `auto_commit_on_turn_end` should ever default to true (leaning no — always require explicit commit or rollback)
- Whether the transaction audit log (SQLite) should be queryable via a separate CLI tool (`nzc txn list`, `nzc txn rollback <id>`)
- What the retention policy is for committed transaction backups (delete immediately? archive for 7 days? configurable?)

---

## Risk Assessment

**Without this system (current state):** Every dangerous file operation by an agent is one mistake away from a config corruption incident. The 2026-03-30 incident will repeat.

**With Level 0 (backup copy):** Config file corruption is recoverable. Single largest risk is eliminated. Cost: ~1 week of implementation.

**With Level 2 (native overlayfs):** Even script-level operations (installers, config tools run via exec) are recoverable. Nearly all local-host agent mistakes are reversible. Cost: ~3 weeks, 3–6 months out.

**Persistent risk:** Remote host operations. ZFS snapshot guard helps for ZFS hosts. Non-ZFS remote hosts (plain ext4 VPS, etc.) remain unprotected unless the agent manually backs up remote files. This risk should be documented in the system prompt and surfaced to the agent before SSH operations.

---

## One-Line Summary

Build Proposal A (explicit transaction API) + Level 0 (backup copy) for Session 3. Ship with `mode = "suggest"` auto-protection. Upgrade to Level 2 (native overlayfs via bwrap) after Session 3. Add Proposal B (PolyClaw meta-commands) when the PolyClaw adapter layer exists. Configure ZFS snapshot guards for known ZFS hosts. Accept that non-ZFS remote hosts are a persistent gap and document it clearly.
