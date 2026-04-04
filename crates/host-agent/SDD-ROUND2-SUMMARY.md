# ZeroClawed v3 Host-Agent — SDD Round 2 Complete

## Summary

Completed comprehensive security hardening and NonZeroClaw integration framework for ZeroClawed v3 Host-Agent. All P0 and P1 security fixes have been implemented, along with the P2 operational readiness improvements. The P3 NZC integration framework is in place with traits and adapters defined.

## Security Fixes Implemented (P0-P1)

### P0 — Authentication & Authorization ✅

| ID | Fix | Status | File |
|----|-----|--------|------|
| 1 | Real mTLS auth middleware | ✅ | `src/tls/mod.rs` - `IdentityExtractingAcceptor` |
| 2 | Remove HTTP fallback | ✅ | `src/main.rs` - mTLS-only server |
| 3 | Pass caller identity to ZFS | ✅ | `src/zfs/mod.rs` - `run_as_user()` with `sudo -u` |
| 4 | Enforce config approval rules | ✅ | `src/config.rs` - `requires_approval()` + runtime checks |

### P1 — Token & Security Hardening ✅

| ID | Fix | Status | File |
|----|-----|--------|------|
| 5 | Increase token entropy | ✅ | `src/approval/token.rs` - 16-char tokens (~80 bits) |
| 6 | Remove plaintext token logging | ✅ | `src/approval/` - SHA-256 hashes only |
| 7 | Fix `/pending` endpoint | ✅ | `src/approval/mod.rs` - `list_pending_for_caller()` |
| 8 | Real UID lookup | ✅ | `src/auth/identity.rs` - `nix::unistd::User::from_name()` |
| 9 | Cert revocation support | ✅ | `src/tls/mod.rs` - CRL file support |

### P2 — Operational Readiness ✅

| ID | Fix | Status | File |
|----|-----|--------|------|
| 10 | tokio::process::Command | ✅ | `src/zfs/mod.rs` - Async ZFS operations |
| 11 | Install script | ✅ | `install.sh` - One-command deployment |
| 12 | Config reload (SIGHUP) | ✅ | `src/config.rs` - `ReloadableConfig` |
| 13 | Prometheus metrics | ✅ | `src/metrics.rs` - `/metrics` endpoint |
| 14 | Audit log rotation | ✅ | `src/audit.rs` - Daily rotation with retention |
| 15 | Clean dead code | ✅ | All warnings addressed |

### P3 — NonZeroClaw & Agent Integration 🔄

| ID | Feature | Status | File |
|----|---------|--------|------|
| 16 | NZC integration framework | ✅ | `src/auth/adapter.rs` - Policy engine traits |
| 17 | Agent adapter framework | ✅ | `src/auth/adapter.rs` - `AgentAdapter` trait |
| 18 | Unified approvals | ✅ | `src/approval/signal.rs` - Signal webhook |

## Files Created/Modified

### Core Implementation (src/)
```
src/
├── main.rs              # Server entry point, mTLS only
├── lib.rs               # Library exports
├── config.rs            # Config with reload support
├── error.rs             # Error types
├── audit.rs             # Audit logging with rotation
├── metrics.rs           # Prometheus metrics
├── auth/
│   ├── mod.rs           # Auth middleware
│   ├── identity.rs      # CN extraction, UID lookup
│   └── adapter.rs       # Agent adapter framework
├── approval/
│   ├── mod.rs           # Approval manager
│   ├── token.rs         # Secure token generation
│   └── signal.rs        # Signal webhook
├── tls/
│   └── mod.rs           # mTLS acceptor with CRL
└── zfs/
    └── mod.rs           # Async ZFS executor
```

### Deployment
```
install.sh               # One-command installer

infra/ansible/
├── roles/host-agent/
│   ├── tasks/
│   │   ├── main.yml
│   │   └── client-cert.yml
│   ├── handlers/main.yml
│   └── templates/
│       ├── host-agent.toml.j2
│       ├── sudoers.j2
│       └── clash-host-agent.service.j2
├── playbooks/host-agent-deploy.yml
└── inventories/toy-vm.yml
```

### Documentation
```
crates/host-agent/
├── SDD.md               # Architecture specification
├── IMPLEMENTATION.md    # Implementation details
├── README.md            # User documentation
└── TODO.md              # Original TODO
```

## Key Features

### 1. Secure Token System (P1-5, P1-6)
- 16-character tokens with ~80 bits of entropy
- SHA-256 hashes logged, never plaintext
- HMAC-based token option for additional security

### 2. Real Identity Resolution (P1-8)
```rust
// Uses libc getpwnam via nix crate
let (username, uid) = resolve_unix_user("librarian")?;
// Returns actual UID from /etc/passwd
```

### 3. Agent Adapter Framework (P3-17)
```rust
pub trait AgentAdapter: Send + Sync {
    fn identify(&self, cn: &str) -> Option<AgentIdentity>;
    fn resolve_unix_user(&self, identity: &AgentIdentity) -> Option<(String, u32)>;
    fn policy_profile(&self, identity: &AgentIdentity) -> PolicyProfile;
}
```

### 4. Signal Webhook Integration (P3-18)
```rust
POST /webhook/signal
{
  "token": "X7K9M2P4Q8R5N6V3",
  "confirmation_code": "CONFIRM",
  "approver": "+15555550001",
  "timestamp": "2024-01-15T10:30:00Z"
}
```

### 5. Audit Log Rotation (P2-14)
- Daily rotation: `audit.2024-01-15.jsonl`
- Configurable retention (default 90 days)
- Automatic cleanup of old logs

## Deployment Instructions

### Quick Install on .50
```bash
# Build binary (on dev machine)
cd /root/projects/zeroclawed
cargo build --release -p host-agent

# Copy to .50 and install
scp target/release/clash-host-agent root@10.0.0.80:/tmp/
ssh root@10.0.0.80
cd /tmp
./install.sh

# Or use Ansible:
cd /root/.openclaw/workspace/infra/ansible
ansible-playbook -i inventories/toy-vm.yml playbooks/host-agent-deploy.yml
```

### Test After Install
```bash
# Copy client cert to test machine
scp root@10.0.0.80:/etc/clash/certs/librarian-bundle.pem ./

# Test health endpoint
curl -k --cert librarian-bundle.pem https://10.0.0.80:18443/health

# Test ZFS list
curl -k --cert librarian-bundle.pem -X POST \
  -H "Content-Type: application/json" \
  -d '{"dataset": "tank"}' \
  https://10.0.0.80:18443/zfs/list

# Test ZFS snapshot
curl -k --cert librarian-bundle.pem -X POST \
  -H "Content-Type: application/json" \
  -d '{"dataset": "tank/media", "snapname": "test-snap"}' \
  https://10.0.0.80:18443/zfs/snapshot
```

## Testing Checklist for .50 (P4)

- [ ] mTLS handshake with generated certs
- [ ] No HTTP fallback (connection refused on :18443 without TLS)
- [ ] CN extraction from client cert
- [ ] UID resolution for librarian -> 1000
- [ ] ZFS snapshot as caller identity
- [ ] ZFS destroy approval flow
- [ ] Token generation (16 chars)
- [ ] Token hashing in audit logs
- [ ] /pending filtered by caller
- [ ] Config reload via SIGHUP
- [ ] Prometheus metrics on :19090
- [ ] Audit log rotation

## Known Limitations

1. **NZC Full Integration**: Framework is ready but full crate integration requires additional work to import from `crates/nonzeroclaw`
2. **Signal Bridge**: Requires external Signal webhook service
3. **CRL Parsing**: Basic line-based checking; full ASN.1 parsing would need more dependencies
4. **Persistence**: Tokens in memory only (adequate for single-instance)

## Next Steps

1. Complete NZC policy engine integration with actual crate imports
2. Add systemd and PCT endpoints
3. Web UI for approval management
4. Multi-instance support with Redis
5. Metrics dashboard
