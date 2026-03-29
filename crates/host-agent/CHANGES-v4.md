# Host-Agent v4 — Adapter-First Refactor

**Branch:** `host-agent-v3`  
**Date:** 2026-03-29  
**Timebox:** ~6 hours  

---

## Summary

v4 introduces an adapter-first architecture that replaces per-operation HTTP handlers
with a single unified dispatch point (`POST /host/op`).  Five adapters are implemented:
ZFS (refactored), Systemd, PCT, Git, and Exec/Ansible (stub).  All legacy ZFS endpoints
are kept as backwards-compatible shims.

---

## What Changed

### 1. Core Architecture (`src/adapters/`)

#### Adapter Trait (`mod.rs`)
```rust
#[async_trait]
pub trait Adapter: Send + Sync {
    fn kind(&self) -> &'static str;
    async fn validate(&self, state: &AppState, op: &HostOp) -> Result<PolicyDecision, AppError>;
    async fn execute(&self, state: &AppState, identity: &ClientIdentity, op: &HostOp) -> Result<ExecutionResult, AppError>;
}
```

**`HostOp`** — unified operation request:
```json
{"kind": "zfs", "resource": "tank/media", "args": ["snapshot", "daily-2026"], "metadata": {}}
{"kind": "systemd", "resource": "nginx.service", "args": ["status"]}
{"kind": "pct", "resource": "101", "args": ["status"]}
{"kind": "git", "resource": "/srv/myapp", "args": ["status"]}
```

**`PolicyDecision`** — `Allow | RequiresApproval { message } | Deny { reason }`

**`ExecutionResult`** — `output: String, exit_code: i32, metadata: HashMap`

#### AdapterRegistry (`registry.rs`)
- `AdapterRegistry::new().with(adapter)` builder pattern
- `dispatch(kind) → Option<Arc<dyn Adapter>>` — logs a warning for unknown kinds
- Thread-safe via `Arc<HashMap>`

### 2. Adapters

#### ZfsAdapter (`zfs.rs`)
- Wraps existing `ZfsExecutor` behind the `Adapter` trait
- Supports: `list`, `snapshot`, `destroy`, `get`, `rollback`
- Reuses `is_valid_dataset_name()` / `is_valid_snapshot_name()` validation
- Policy lookup: maps `args[0]` to `zfs-{command}` rule key

#### SystemdAdapter (`systemd.rs`)
- Operations: `status`, `start`, `stop`, `restart`
- Service name validation: strict regex `^[a-zA-Z0-9_][a-zA-Z0-9_\-.@]*\.(service|socket|timer|target|mount|path)$`
- Rejects: path traversal, shell metacharacters, bare names without suffix
- Execution: `sudo /usr/bin/systemctl {command} {service}`
- `status` returns output even for non-zero exit (inactive services)

#### PctAdapter (`pct.rs`)
- Operations: `status`, `start`, `stop`, `destroy`
- VM ID validation: numeric, range 100–999999
- `destroy` always requires approval (unconditional, not configurable off)
- `start`/`stop` require approval by default unless explicitly configured
- Execution: `sudo /usr/sbin/pct {command} {vmid}`

#### GitAdapter (`git.rs`)
- Operations: `status`, `fetch`, `pull`, `checkout`, `log`
- Branch name validation: rejects `..`, leading `-`, shell metacharacters
- Repo path validation: must be absolute, no `..` components, must exist on disk
- Repo allowlist in `config.git.allowed_repos` (empty = all allowed)
- No shell interpolation: all argv tokens passed separately
- Execution: `git {command} [branch]` in `current_dir(repo_path)`

#### ExecAdapter (`exec.rs`)
- **Disabled by default** — requires `exec.enabled = true` in config
- Operations: `run` only
- Command allowlist: `exec.allowed_commands = ["/usr/bin/uptime", ...]`
- Ansible stub: detects `ansible://playbook.yml` resource, writes job spec to queue dir
- No shell interpolation

### 3. Unified Dispatch Endpoint

**`POST /host/op`** — new in v4:
```
Flow:
  1. Look up adapter by kind
  2. adapter.validate() → PolicyDecision
  3. If Deny → 403 with reason
  4. If RequiresApproval + token present → consume token, execute
  5. If RequiresApproval + no token → create approval request, return pending JSON
  6. If Allow → execute
  7. Audit log + return ExecutionResult
```

Legacy endpoints (`/zfs/snapshot`, `/zfs/list`, `/zfs/destroy`) remain and continue to work.

### 4. Config Changes (`config.rs`)

New sections in `Config`:

```toml
[git]
allowed_repos = ["/srv", "/opt"]

[exec]
enabled = false
allowed_commands = []
# ansible_job_queue = "/var/lib/clash/ansible-jobs"
```

New default rules (for `systemd-*`, `pct-*`, `git-*`, `zfs-rollback`):

| Operation | approval_required | always_ask |
|-----------|-------------------|------------|
| systemd-status | false | false |
| systemd-start | false | false |
| systemd-stop | false | false |
| systemd-restart | false | false |
| pct-status | false | false |
| pct-start | true | false |
| pct-stop | true | false |
| pct-destroy | true | true (admin only) |
| git-status | false | false |
| git-log | false | false |
| git-fetch | false | false |
| git-pull | false | false |
| git-checkout | true | false |
| zfs-rollback | true | true |

### 5. Dependency Added

```toml
async-trait = "0.1"
```

---

## Sample Policy Rules (TOML)

```toml
# Allow librarian to restart services without approval
[[rules]]
operation = "systemd-restart"
approval_required = false

# Require approval to stop any service matching "critical-*"
[[rules]]
operation = "systemd-stop"
approval_required = true
pattern = "critical-.*\\.service"

# PCT destroy always requires admin approval
[[rules]]
operation = "pct-destroy"
approval_required = true
always_ask = true
approval_admin_only = true

# Git checkout requires approval
[[rules]]
operation = "git-checkout"
approval_required = true
always_ask = false

# Allow git pull in /srv repos only (enforced via git.allowed_repos)
[[rules]]
operation = "git-pull"
approval_required = false
```

---

## Sudoers Configuration (add to `/etc/sudoers.d/host-agent`)

```sudoers
# Systemd
clash-agent ALL=(root) NOPASSWD: /usr/bin/systemctl status *.service
clash-agent ALL=(root) NOPASSWD: /usr/bin/systemctl start *.service
clash-agent ALL=(root) NOPASSWD: /usr/bin/systemctl stop *.service
clash-agent ALL=(root) NOPASSWD: /usr/bin/systemctl restart *.service
clash-agent ALL=(root) NOPASSWD: /usr/bin/systemctl status *.timer
clash-agent ALL=(root) NOPASSWD: /usr/bin/systemctl status *.socket

# PCT (non-destructive only by default)
clash-agent ALL=(root) NOPASSWD: /usr/sbin/pct status *
clash-agent ALL=(root) NOPASSWD: /usr/sbin/pct start *
clash-agent ALL=(root) NOPASSWD: /usr/sbin/pct stop *
```

---

## Test Results

### Unit Tests: 96 passed, 0 failed

| Module | Tests |
|--------|-------|
| adapters::registry | 4 |
| adapters::zfs | 8 |
| adapters::systemd | 3 |
| adapters::pct | 3 |
| adapters::git | 4 |
| adapters::exec | 3 |
| (all existing tests) | 72 |

### Integration Smoke Tests (10.0.0.80)

| Test | Result |
|------|--------|
| `/health` — mTLS verified | ✅ PASS |
| `POST /host/op` zfs list | ✅ PASS (4 datasets returned) |
| `POST /host/op` systemd status cron.service | ✅ PASS |
| `POST /host/op` pct status 100 (no CT exists) | ✅ PASS (graceful error) |
| `POST /host/op` pct status 99 (invalid vmid) | ✅ PASS (denied with reason) |
| `POST /host/op` git status /nonexistent | ✅ PASS (denied: path not exist) |
| `POST /host/op` git checkout 'main; rm -rf /' | ✅ PASS (shell injection blocked) |
| `POST /host/op` exec run /usr/bin/uptime (disabled) | ✅ PASS (denied: adapter disabled) |
| `POST /host/op` zfs destroy (no token) | ✅ PASS (approval pending returned) |
| Legacy `POST /zfs/list` | ✅ PASS (backwards compat) |

---

## Outstanding TODOs

1. **Git adapter actual integration test**: needs a real git repo on .50 and the repo in the `allowed_repos` config.  Current test confirms path validation works, but git subprocess wasn't exercised.

2. **Systemd sudoers on .50**: the `sudo /usr/bin/systemctl` smoke test passed because clash-agent on .50 already has broad sudo.  In production, scope the sudoers entries to the specific service patterns.

3. **PCT sudoers**: similarly, `sudo /usr/sbin/pct` needs scoped sudoers in production.

4. **Config TOML update for git/exec**: the deployed config on .50 doesn't yet have `[git]` or `[exec]` sections.  The defaults (git: no allowlist restriction, exec: disabled) are applied from code defaults.

5. **ExecAdapter full implementation**: the Ansible job queue stub writes job specs to a directory.  An actual Ansible runner / queue consumer is not implemented.

6. **Rate limiter for /host/op**: currently the rate limiter uses the `/zfs/destroy` endpoint list.  The new `/host/op` endpoint should be added to `rate_limit.endpoints` in config.

7. **Metrics per adapter**: `increment_zfs_operation()` is still called only from legacy endpoints.  New adapter dispatch path should increment per-kind metrics.

8. **SIGHUP config reload**: works for base config; `GitConfig`/`ExecConfig` are now included in the reloaded config.

---

## Commit History

```
434ffec docs(adapter): add ADAPTERS.md design note
2b48510 feat(adapter): introduce HostOp, Adapter trait, AdapterRegistry, all 5 adapters + /host/op dispatch
b1be427 feat(deploy): update certs + deploy v4 binary to 10.0.0.80
```
