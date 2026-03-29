# Host-Agent Round 4 Changes

## Commit: fix(host-agent): Round 4 security hardening + P0/P1 bug fixes

### A. Mandatory Functional Fixes

#### P0-A1: Wire auth middleware — ClientIdentity injected via custom accept loop
- **Problem**: `axum_server::bind_rustls` bypassed `IdentityExtractingAcceptor`; handlers using `Extension<ClientIdentity>` would panic (500) on every request.
- **Fix**: Replaced `axum_server::bind_rustls` with a manual `tokio::net::TcpListener` accept loop. Each connection goes through `IdentityExtractingAcceptor::accept()` which performs TLS handshake + extracts peer cert + builds `ClientIdentity`. A `tower::service_fn` wrapper injects the identity into request extensions before dispatching to axum. Wrapped in `TowerToHyperService` for hyper compatibility.
- **Effect**: All authenticated handlers now receive `ClientIdentity` correctly. Unauthenticated connections are rejected at TLS handshake (no client cert = TLS failure). `auth_middleware` provides belt-and-suspenders 401 for any path that somehow bypasses extraction.

#### P0-A2: Fix zfs_destroy control flow — three explicit branches
- **Problem**: The previous logic had a fallthrough bug: when `requires_approval=false` and a token was provided, it would fall through to a confusing `PolicyDenied` error.
- **Fix**: Three distinct, mutually exclusive early-return branches:
  1. `requires_approval=false` → execute immediately, return `DestroyResponse::Success`
  2. `approval_token` provided → validate/consume token, execute or return `InvalidToken`
  3. No token + `requires_approval=true` → create approval, return `DestroyResponse::Pending`
- **Tests**: See `config::tests::test_full_autonomy_*` for control-flow coverage.

#### P0-A3: /approve requires approver identity check (configurable)
- **New config fields** (`[approval]` section):
  - `admin_cn_pattern = "librarian*"` — if set, only clients with matching CN can approve operations with `approval_admin_only = true` on the rule.
  - `identity_plugin = "command:/path/to/bin"` or `"http://127.0.0.1:PORT/validate"` — optional out-of-process hook for approver identity validation (see P-C7 below).
- **Default**: Any mTLS-authenticated client can approve (existing behavior preserved). Only operations with `approval_admin_only = true` in `[[rules]]` trigger the admin CN check.
- **Handler**: `submit_approval` now takes `Extension<ClientIdentity>` (requires auth, returns 401 without cert).

### B. High Priority Hardening

#### P-B4: FullAutonomy cannot bypass `always_ask = true` operations
- **New `RuleConfig` field**: `always_ask = false` (default). When `true`, no agent — not even `Full` autonomy — can skip the approval requirement.
- **New `AgentConfig` field**: `allow_full_autonomy_bypass = false` (default). Must be explicitly set `true` in TOML per-agent to enable bypass for non-`always_ask` operations.
- **Default config**: `zfs-destroy` has `always_ask = true`. No agent can bypass it without an explicit rule change.
- **Tests**: `test_full_autonomy_cannot_bypass_always_ask`, `test_full_autonomy_bypass_when_explicitly_enabled`, `test_supervised_always_requires_approval`.

#### P-B5: Per-CN token-bucket rate limiter for destructive endpoints
- **New module**: `crates/host-agent/src/rate_limit.rs`
- **Implementation**: `DashMap`-based per-CN token bucket. No global lock; each CN gets an independent shard. Window-based: 5 requests/60s default.
- **Configurable** via `[rate_limit]` TOML section: `enabled`, `max_requests`, `window_seconds`, `endpoints`.
- **Applied to**: `/zfs/destroy`, `/approve`, `/pending` by default.
- **Metrics**: Increments `host_agent_rate_limited_total` Prometheus counter on denial.
- **Returns**: HTTP 429 with `Retry-After` header.
- **Tests**: 6 unit tests covering limits, independent CNs, window reset, disabled mode, path matching.

### C. Nice-to-Have

#### P-C6: Constant-time token hash comparison
- `verify_token_hash()` now uses `subtle::ConstantTimeEq` for hash comparison.
- Prevents timing-based oracle attacks where an adversary could infer how many leading bytes of their hash guess are correct.
- **Remaining timing risk** (documented in `token.rs`): HashMap key lookup itself is not constant-time, but the key is a 256-bit secret hash derived from the token — so timing of "found vs. not found" does not leak the plaintext token.

#### P-C7: Identity plugin extension point for approvals
- **New module**: `crates/host-agent/src/approval/identity_plugin.rs`
- **Config**: `approval.identity_plugin = "command:/path/to/bin"` or `"http://127.0.0.1:PORT/validate"`
- **Protocol**: 
  - stdin: `{"approver_cn":"...", "approval_id":"...", "operation":"...", "target":"..."}`
  - stdout: `{"allowed": true|false, "reason": "optional"}`
- **Security**: Command plugins must use absolute paths (relative paths rejected to prevent PATH hijacking). HTTP plugins have 5-second timeout. Non-zero exit code or invalid JSON → deny (fail-closed).
- **Default**: Disabled (no config entry → skip plugin, mTLS is sole gate).
- **Tests**: 5 unit tests covering allow/deny/invalid-JSON/missing-reason/relative-path guard.

### D. Test Results

```
test result: ok. 48 passed; 0 failed; 0 ignored
```

Release build: `cargo build --release -p host-agent` ✅

### E. Configuration Example

```toml
[rate_limit]
enabled = true
max_requests = 5
window_seconds = 60
endpoints = ["/zfs/destroy", "/approve", "/pending"]

[approval]
enabled = true
ttl_seconds = 300
admin_cn_pattern = "librarian*"    # only this CN pattern can approve admin_only ops
identity_plugin = "command:/etc/clash/plugins/verify-approver"  # optional

[[rules]]
operation = "zfs-destroy"
approval_required = true
always_ask = true          # Full autonomy CANNOT bypass this
approval_admin_only = false

[[agents]]
cn_pattern = "automation*"
agent_type = "automation"
unix_user = "clash-agent"
autonomy = "full"
allow_full_autonomy_bypass = false  # default safe; set true to allow bypass of non-always_ask ops
```

### F. Not Completed / Timebox Exceedances

None within the defined scope. All P0/P1/P-B/P-C items were implemented within the 90-minute timebox.
