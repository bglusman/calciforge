# Vault Integration + NonZeroClawed Native OpenClaw Adapter — Planning Notes

_Created: 2026-03-30 by Librarian_
_Context: Post-incident planning after a bad OpenClaw config edit broke Librarian entirely_

---

## Overview

Two related but distinct workstreams:

1. **Vault Integration** — credential management via Bitwarden/Vaultwarden (or adapters)
2. **NonZeroClawed Native OpenClaw Adapter** — safe, versioned, installation-time integration with downstream OpenClaw instances

These belong together because the adapter installation story is where vault-stored credentials (SSH keys, API keys generated during setup) first come into play.

---

## Workstream 1: Vault Integration

### Motivation
- Credentials currently stored as plaintext files (`~/credentials/`) in agent workspaces
- API keys live in agent context windows — extractable, loggable, prompt-injectable
- Bitwarden already in use; natural to make it the canonical secrets store
- Bitwarden Agent Access SDK (launched 2026-03-30) + OneCLI provides a reference integration

### Design: Adapter Interface

```rust
trait VaultAdapter {
    async fn get_secret(&self, key: &str) -> Result<Secret>;
    async fn store_secret(&self, key: &str, value: SecretValue) -> Result<()>;
    async fn request_approval(&self, key: &str, context: &str) -> Result<ApprovalToken>;
}
```

Implementations:
- **BitwardenAdapter** (default) — via Bitwarden Agent Access SDK or direct API
- **VaultwardenAdapter** — self-hosted, same API surface as Bitwarden (verify parity with Agent Access SDK before building)
- **HashiCorpVaultAdapter** — for enterprise/existing infra
- **AnsibleVaultAdapter** — for simpler setups
- **EnvAdapter** — fallback/dev, reads from environment (no approval flow)

### Vaultwarden as Managed Service (not embedded)
Vaultwarden is a full server — don't embed. Instead:
- If installer detects no existing vault configured → offer to spin up Vaultwarden as Docker container or systemd unit
- Installer handles initial admin setup, stores master credentials somewhere bootstrap-safe
- This gives "batteries included" without linking in a full server binary

### SSH Key Generation Flow (installer)
Today: generate keypair → dump to disk → user manually stores  
Future:
1. Generate keypair in memory
2. Store private key directly in vault via adapter
3. Store public key where needed (authorized_keys, etc.)
4. Private key never touches disk unencrypted

### Approval Flow for Agents
Bitwarden's SDK approval is CLI-based — doesn't fit async agent workflows.
Need: approval requests delivered over configured channel (Signal/Telegram), user approves in chat.
- NZC/NonZeroClawed owns the approval relay (it's the channel layer)
- OneCLI or NZC intercepts outbound API calls, checks if credential needs approval, relays request

### Approval Policy — Per-Secret, Not Global

Approval is **optional and configurable per secret**, not a global toggle. Policy options:

```toml
[vault.secrets.anthropic_api_key]
policy = "auto"          # inject silently, no human approval needed

[vault.secrets.stripe_key]
policy = "per-use"       # approve every time

[vault.secrets.github_token]
policy = "session"       # approve once per conversation session

[vault.secrets.deploy_key]
policy = "time-bound"    # approve once, valid for N hours
ttl = "4h"
```

Default: `auto` for most secrets (security improvement without friction). `per-use`/`session` for sensitive externally-facing APIs.

### Vault Session Token (no master password in memory)

The vault master password never stays in memory long-term. Pattern:
1. Vault unlocked at startup (or first secret request) → short-lived session token cached
2. All secret fetches use session token, not master password
3. Token expires → re-unlock automatically (if unlock key stored) or prompt user over channel
4. Bitwarden CLI already works this way: `bw unlock` → session key → TTL-scoped access

NZC/NonZeroClawed holds the session token only. Master password handled at unlock time.

### Phase 1: No Agent Access SDK Required

Use `bw` CLI subprocess for vault access first. Build the approval relay directly in NonZeroClawed.
Agent Access SDK integration can come later once the core flow is proven.

### Open Questions
- [ ] Does Vaultwarden support Bitwarden's Agent Access SDK endpoints? Verify before building
- [ ] What's the OneCLI license and embedding story — is it a dependency or a sidecar?
- [x] Approval UX: **per-secret policy** (auto/session/per-use/time-bound) — decided above

---

## Workstream 2: NonZeroClawed Native OpenClaw Adapter

### Motivation
- NonZeroClawed needs to integrate with downstream OpenClaw instances (add channel, configure routing)
- This requires editing OpenClaw's config — which is **dangerous if done wrong**
- 2026-03-30: Librarian tried to add a NonZeroClawed channel adapter manually → broke config → gateway crashed → Custodian couldn't fix it → required manual recovery

### Core Principle
> **Config changes to a downstream claw must be initiated by NonZeroClawed's installer, not by the claw itself.**

The claw should never edit its own config in response to a NonZeroClawed integration request. NonZeroClawed owns that process.

### Safety Requirements (non-negotiable)

1. **Backup first** — always snapshot `openclaw.json` before any modification
2. **Version check** — read OpenClaw version, check against known-compatible schema versions
3. **Schema validation** — use `openclaw config schema` (or equivalent) to validate proposed changes before writing
4. **Dry run** — show diff of what will change, get explicit confirmation before applying
5. **Rollback path** — keep backup, know how to restore if gateway fails to start
6. **One claw at a time** — never modify two claws in the same operation (safety circuit breaker)
7. **Health check after** — verify gateway comes back up before declaring success

### OpenClaw Version Compatibility Problem
Different claw instances may run different OpenClaw versions with different config schemas.
- Librarian: 2026.3.13
- Lucien: may differ
- Future installs: unknown

Adapter must:
- Read version before touching anything
- Maintain a compatibility matrix (or query schema dynamically)
- Refuse to proceed if version is unknown/unsupported
- Never guess at config field names

### Proposed Adapter Flow

```
nonzeroclawed install-adapter --target <claw-endpoint> --adapter openclaw
  1. Connect to target claw gateway
  2. GET /version — check compatibility
  3. GET /config — read current config
  4. Backup config locally + on target
  5. Generate proposed config diff
  6. Show diff to operator, require explicit approval
  7. POST /config (or edit file + restart) — apply change
  8. Health check: poll /up for 30s
  9. On failure: automatic rollback from backup + alert
```

### What the Adapter Actually Adds
Minimal footprint — just enough for NonZeroClawed to receive messages from/send to the claw:
- Add a `nonzeroclawed` channel entry (once OpenClaw supports this natively — don't hack it in)
- OR: configure a webhook/hook endpoint that NonZeroClawed calls
- The right shape depends on what OpenClaw exposes; check docs/schema before designing

**Key lesson from 2026-03-30:** Don't try to add config keys that don't exist in the running version's schema. This is what broke things.

### Relation to Vault
During adapter installation, any credentials generated (e.g. a shared secret between NonZeroClawed and the claw) should be stored in vault, not written to a config file or handed back in plaintext.

---

## Suggested Research Tasks for Initial Session

1. **Vaultwarden + Agent Access SDK parity** — does Vaultwarden implement the endpoints the SDK needs? Check GitHub issues/docs.
2. **OneCLI architecture** — is it a standalone proxy, a library, or both? What's the embedding story for NZC?
3. **OpenClaw plugin/channel API** — what's the right hook point for NonZeroClawed integration? Webhook? Native channel plugin? Check OpenClaw docs/source.
4. **OpenClaw config schema versioning** — is there a machine-readable compatibility manifest? How do we detect version safely?
5. **Approval relay design** — sketch the flow for "agent requests credential → NonZeroClawed relays approval request to user → user approves in Signal → credential released"

---

## Workstream 3: OpenClaw → NonZeroClawed/NZC Migration

### Motivation
Many early adopters will be coming from OpenClaw. Installation is the time to make key decisions and migrate what makes sense. This is less "copy everything over" and more "assign ownership and import history."

### Memory Storage Correction
- **NZC**: SQLite for session/memory storage
- **OpenClaw**: plain text files (`~/.openclaw/workspace/memory/`, `MEMORY.md`, daily markdown files)

Migration direction: OpenClaw's markdown memory is human-readable and straightforward to import into NZC's SQLite. No schema reverse-engineering needed — parse markdown, insert into NZC store.

Note: NZC memory architecture is also under consideration for redesign (more dynamic/relevance-based injection scanning conversation context). Migration just gets the content in; the new system decides how to use it.

### Channel Assignment (not migration)
Each channel has exactly one owner at a time: OpenClaw, NonZeroClawed, or NZC. This is a **decision**, not a copy operation.

Installation question: "Which channels do you want NonZeroClawed to own?"
- NonZeroClawed-owned channels: NonZeroClawed is the router, downstream claws are receivers
- OpenClaw-owned channels: OpenClaw handles them directly, unchanged
- NZC-owned channels: NZC handles them directly

The installer walks through each configured channel and asks who owns it. Credentials for NonZeroClawed-owned channels get moved to vault. OpenClaw's config for those channels gets disabled (safely, with backup).

### Context Continuity on Channel Reassignment
If a user later reassigns a channel from NZC back to OpenClaw (or vice versa), NonZeroClawed handles it:
- User requests reassignment ("switch Signal back to OpenClaw")
- NonZeroClawed updates its routing table
- NonZeroClawed passes relevant recent history to the newly-assigned claw so it's not starting cold
- No transparent runtime fallback — explicit manual reassignment with context handoff

### Memory Migration
- Read OpenClaw markdown memory files (`MEMORY.md`, `memory/*.md`, daily files)
- Import into NZC SQLite with appropriate metadata (date, source file)
- Keep originals untouched
- Flag anything that didn't import cleanly for manual review

### Config Migration
- Read `openclaw.json`, map known fields to NZC/NonZeroClawed equivalents
- Fields with clear analogs: models, agent list, hooks, approval targets
- Channels: don't migrate directly — handled by channel assignment step above
- Fields that don't map: flagged for manual review, never silently dropped
- Show as proposed diff, require explicit confirmation

### Installer UX (proposed)
```
nonzeroclawed install
  → Detect existing OpenClaw installation? [yes/no]
  → If yes:
      → Channel assignment: for each channel, who owns it? (NonZeroClawed / OpenClaw / NZC)
      → Memory import: import OpenClaw markdown memory into NZC? [yes/no]
      → Config migration: show diff of mapped settings, confirm
      → Vault setup: store credentials for NonZeroClawed-owned channels
      → Disable migrated channels in OpenClaw config (with backup)
      → Health check all active claws
```

### Open Questions
- [ ] What's the right data format for NonZeroClawed's routing table (channel → owner)?
- [ ] How does NonZeroClawed pass history context on channel reassignment — full dump or summarized?
- [ ] NZC memory redesign status — should import target the current schema or wait for the new one?
- [ ] What OpenClaw config fields map cleanly to NZC equivalents? (needs NZC config schema reference)

---

## Workstream 4: Filesystem Transactions (Research Task)

_Captured 2026-03-30. Design/research only — not in current 4-session sprint._

### Motivation
Dangerous operations (config edits, migrations, installer steps) need rollback capability. ZFS snapshots work at volume level but require ZFS. jai's copy-on-write overlay could provide "filesystem transaction" semantics at the process level: enter jail → operate → validate → commit or rollback.

### The Core Idea
```
transaction_start()   → enter jai jail (COW overlay active)
  ... do dangerous file operations ...
  ... validate result (health check, config parse, etc.) ...
transaction_commit()  → replay overlay diff onto real filesystem
transaction_rollback() → discard overlay, originals untouched
```

Semantically: filesystem SQL transaction. Rollback is free (discard overlay). Commit requires iterating overlayfs upper dir and applying changes to real filesystem — non-trivial (permissions, symlinks, deletions).

### Interface Options

**Option A: Tool-call level (NZC)**
- `transaction_start`, `transaction_commit`, `transaction_rollback` as explicit NZC tool calls
- Agent opts in when it knows it's about to do something dangerous
- Pro: explicit, composable, agent-driven
- Con: requires agent to correctly identify dangerous operations — exactly when agents make mistakes
- Mitigation: make it easy enough that agents default to wrapping anything touching config/data files

**Option B: NonZeroClawed meta-commands**
- `!transaction start` / `!transaction commit` / `!transaction rollback` at NonZeroClawed level
- Human or agent invokes; NonZeroClawed coordinates with underlying claw's system
- Pro: human-in-the-loop control point
- Con: must hook into every claw's underlying system; complex; doesn't compose across SSH boundaries

**Option C: Automatic/implicit**
- NZC detects file write operations and auto-wraps in transaction
- No opt-in needed
- Con: false positive rate, detection heuristic hard to get right, unexpected behavior

### The SSH Boundary Problem
jai/overlayfs only protects the local filesystem. When agent SSHs to a remote host (.127, .52, etc.) and edits files there, there is no jai coverage. Options:
- **Caveat clearly**: transactions are local-only; warn explicitly before crossing SSH boundary
- **ZFS snapshot on remote**: before SSH operations on ZFS hosts, take a snapshot first (already our practice for destructive ops)
- **Remote transaction protocol**: too complex, probably not worth it

This is a real limitation. The transaction abstraction is most valuable for local installer/config operations, less so for distributed operations.

### Backend Separation
The *interface* (tool calls / meta-commands) is separable from the *backend*:
- **Backend v1 (simplest):** backup copy before touching any file. `cp openclaw.json openclaw.json.bak.$(date +%s)`. No jai required.
- **Backend v2 (jai/overlayfs):** proper COW, commit by replaying upper dir. Better for multi-file operations where you want atomic all-or-nothing.
- **Backend v3 (ZFS):** snapshot before, destroy after commit. Already works remotely if host has ZFS.

Could ship v1, upgrade to v2/v3 as needed. The interface stays the same.

### jai as Backend — Open Questions
- Does jai expose an API for reading the overlayfs upper dir (the diff)?
- Is there a `jai commit` concept or would we build it on top?
- Likely: jai doesn't have this (designed for "run and discard"), so we'd implement commit logic ourselves
- Worth reading jai source: https://github.com/stanford-scs/jai

### NZC Existing Sandbox Infrastructure
- `sandbox-bubblewrap` feature flag already in NZC's `Cargo.toml`
- `sandbox-landlock` feature flag also present
- These are the same primitives jai uses — NZC could implement this natively rather than depending on jai binary

### Decision Point for Current Sprint
Session 3 (NonZeroClawed adapter installation) is the highest-risk session — it edits live OpenClaw config. For that session, use **Backend v1** (backup copy) as the safety mechanism. That's already in the safety requirements. The fuller transaction abstraction can come after.

### Research Tasks
1. Read jai source — does it expose upper dir / commit concept?
2. Benchmark overlayfs setup cost — is it fast enough for per-operation use?
3. Design the tool-call interface in detail — what does `transaction_start` return? How does the agent reference the transaction in `commit`/`rollback`?
4. Define exactly which NZC tool calls should auto-suggest wrapping in a transaction (exec with file writes? gateway config.apply? all write tool calls?)
5. Survey: what do other agent frameworks do here? (Codex, Claude Code, etc.)

---

## References
- Bitwarden Agent Access SDK announcement: https://bitwarden.com/blog/introducing-agent-access-sdk/
- OneCLI + Bitwarden integration: https://www.onecli.sh/blog/bitwarden-agent-access-sdk-onecli
- OneCLI GitHub: https://github.com/onecli/onecli
- Bitwarden Agent Access SDK: https://github.com/bitwarden/agent-access
- Incident postmortem: 2026-03-30 Librarian config corruption (ask Librarian for details)
