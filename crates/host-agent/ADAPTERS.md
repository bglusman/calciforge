# Host-Agent Adapter-First Architecture — Design Note

## Motivation

The v3 host-agent had per-operation HTTP handlers (`/zfs/snapshot`, `/zfs/list`, `/zfs/destroy`).
Adding systemd, pct, git, and exec/ansible adapters would have multiplied the number of
routes and duplicated policy-evaluation boilerplate across every handler.

The adapter-first refactor introduces a single dispatch point (`POST /host/op`) and a
well-defined `Adapter` trait. New capabilities are added by implementing one trait.

---

## Architecture

```
POST /host/op
     │
     ▼
HostOp { kind, resource, args, metadata }
     │
     ▼
AdapterRegistry::dispatch(kind) → Box<dyn Adapter>
     │
     ├─ adapter.validate(&AppState, &HostOp) → PolicyDecision
     │       (checks kind/resource/args against policy rules)
     │
     ├─ policy engine: always_ask / autonomy / approval flow
     │
     └─ adapter.execute(&AppState, &ClientIdentity, &HostOp) → ExecutionResult
```

---

## Core Types

### HostOp
```rust
pub struct HostOp {
    pub kind: String,               // "zfs", "systemd", "pct", "git", "exec"
    pub resource: Option<String>,   // dataset, service, vmid, repo-path, command
    pub args: Vec<String>,          // operation-specific args
    pub metadata: HashMap<String, serde_json::Value>,
}
```

### Adapter Trait
```rust
#[async_trait]
pub trait Adapter: Send + Sync {
    fn kind(&self) -> &'static str;
    async fn validate(&self, state: &AppState, op: &HostOp) -> Result<PolicyDecision, AppError>;
    async fn execute(&self, state: &AppState, identity: &ClientIdentity, op: &HostOp) -> Result<ExecutionResult, AppError>;
}
```

### PolicyDecision
```rust
pub enum PolicyDecision {
    Allow,
    RequiresApproval { message: String },
    Deny { reason: String },
}
```

### ExecutionResult
```rust
pub struct ExecutionResult {
    pub output: String,
    pub exit_code: i32,
    pub metadata: HashMap<String, serde_json::Value>,
}
```

---

## Adapters

| Adapter       | Enabled Default | Operations                              | Sudoers                          |
|---------------|-----------------|------------------------------------------|------------------------------------|
| ZfsAdapter    | ✅ yes           | list, snapshot, destroy, rollback, get  | `sudo -u <unix_user> zfs ...`     |
| SystemdAdapter| ✅ yes           | status, start, stop, restart            | `sudo /usr/bin/systemctl ...`      |
| PctAdapter    | ✅ yes           | status, start, stop                     | `sudo /usr/sbin/pct ...`           |
| GitAdapter    | ✅ yes           | status, fetch, pull, checkout           | runs as unix_user, repo allowlist  |
| ExecAdapter   | ❌ disabled      | run (allowlisted commands only)         | command allowlist in config        |

---

## Policy Rule Examples

```toml
[[rules]]
operation_kind = "zfs"
command = "destroy"
resource_pattern = ".*"
approval_required = true
always_ask = true

[[rules]]
operation_kind = "zfs"
command = "list"
resource_pattern = ".*"
approval_required = false

[[rules]]
operation_kind = "systemd"
command = "start"
resource_pattern = ".*\\.service"
approval_required = false

[[rules]]
operation_kind = "systemd"
command = "stop"
resource_pattern = "critical.*\\.service"
approval_required = true
always_ask = true

[[rules]]
operation_kind = "pct"
command = "stop"
resource_pattern = "\\d+"
approval_required = true

[[rules]]
operation_kind = "pct"
command = "destroy"
resource_pattern = ".*"
approval_required = true
always_ask = true

[[rules]]
operation_kind = "git"
command = "status"
approval_required = false

[[rules]]
operation_kind = "git"
command = "pull"
resource_pattern = "/srv/.*"
approval_required = false
```

---

## Legacy Compatibility

The old `/zfs/snapshot`, `/zfs/list`, `/zfs/destroy` routes are kept as thin shims
that translate to `HostOp` and call the unified `host_op_dispatch` handler.
This ensures clients written against v3 continue to work during transition.

---

## Commit Plan

1. `feat(adapter): introduce HostOp, Adapter trait, AdapterRegistry, PolicyDecision`
2. `feat(adapter/zfs): wrap ZfsExecutor behind ZfsAdapter`
3. `feat(adapter/systemd): implement SystemdAdapter`
4. `feat(adapter/pct): implement PctAdapter`
5. `feat(adapter/git): implement GitAdapter`
6. `feat(adapter/exec): implement ExecAdapter (stub, disabled by default)`
7. `feat(dispatch): single /host/op endpoint + legacy shims`
8. `test: unit tests for AdapterRegistry and each adapter validation`
9. `docs: update README + CHANGES-v4.md`
