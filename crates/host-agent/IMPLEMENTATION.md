# SDD Round 2 Implementation Summary

## Overview
Completed comprehensive security hardening and NonZeroClaw integration preparation for ZeroClawed v3 Host-Agent.

## Files Modified/Created

### Core Source Files

1. **src/main.rs** - Complete rewrite with:
   - Proper mTLS-only server (no HTTP fallback)
   - ClientIdentity extraction and injection
   - Signal webhook endpoint
   - Metrics server spawn
   - Clean error handling

2. **src/auth/mod.rs** - New module structure
3. **src/auth/identity.rs** - NEW
   - Real UID lookup via `nix::unistd::User::from_name()` (P1-8)
   - Certificate fingerprint calculation
   - CRL checking support (P1-9)
   - Root mapping rejection

4. **src/auth/adapter.rs** - NEW (P3-17)
   - AgentAdapter trait for extensible identity mapping
   - ConfigAgentAdapter for pattern-based CN matching
   - AgentRegistry with caching
   - PolicyProfile with autonomy levels
   - Support for: Librarian, Lucien, Zeroclaw, ACPX agents

5. **src/tls/mod.rs** - Rewritten
   - IdentityExtractingAcceptor for CN extraction
   - CRL file support in config
   - Proper certificate chain handling

6. **src/approval/mod.rs** - Rewritten
   - 16-character token generation (P1-5)
   - SHA-256 token hashing for logs (P1-6)
   - Signal webhook integration (P3-18)
   - Caller-filtered pending list (P1-7)
   - NZC request ID support

7. **src/approval/token.rs** - NEW (P1-5, P1-6)
   - High-entropy token generation (~80 bits)
   - HMAC-based token option
   - Token hashing for audit logs
   - Token masking for display

8. **src/approval/signal.rs** - NEW (P3-18)
   - Signal webhook payload handling
   - Approval validation
   - Allowed approver list
   - Expiration checking

9. **src/zfs/mod.rs** - Rewritten (P2-10)
   - Async ZFS commands via tokio::process
   - User identity passed via sudo -u (P0-3)
   - Proper error parsing
   - Dataset validation

10. **src/config.rs** - Rewritten (P2-12)
    - Agent configuration support
    - ReloadableConfig with RwLock
    - SIGHUP reload support
    - Pattern-based rules (P0-4)
    - AutonomyLevel enum

11. **src/audit.rs** - Rewritten (P2-14)
    - Daily log rotation
    - Retention policy
    - Token hash field in events

12. **src/metrics.rs** - NEW (P2-13)
    - Prometheus metrics endpoint
    - Counters for requests, ZFS ops, approvals
    - AtomicU64 for thread safety

13. **src/error.rs** - Updated
    - New error types for identity, policy
    - Proper HTTP status mapping

### Build Configuration

14. **Cargo.toml** - Updated dependencies
    - Added: hmac, sha2, hex, nix
    - Updated: tokio with process feature
    - Version bumped to 0.2.0

### Deployment

15. **install.sh** - NEW (P2-11)
    - One-command installation
    - Certificate generation
    - User/directory setup
    - Systemd service creation
    - Installation testing

16. **Ansible role** - NEW
    - `roles/host-agent/tasks/main.yml`
    - `roles/host-agent/tasks/client-cert.yml`
    - `roles/host-agent/handlers/main.yml`
    - `roles/host-agent/templates/host-agent.toml.j2`
    - `roles/host-agent/templates/sudoers.j2`
    - `roles/host-agent/templates/clash-host-agent.service.j2`

17. **Ansible playbook** - NEW
    - `playbooks/host-agent-deploy.yml`
    - `inventories/toy-vm.yml`

### Documentation

18. **README.md** - Complete rewrite
19. **SDD.md** - Architecture specification
20. **IMPLEMENTATION.md** - This file

## Security Fixes Checklist

### P0 — Authentication & Authorization ✅
- [x] Real mTLS auth middleware with CN extraction
- [x] HTTP fallback removed - TLS failure is fatal
- [x] Caller identity passed to ZFS via sudo -u
- [x] Config approval rules enforced at runtime

### P1 — Token & Security Hardening ✅
- [x] 16-character tokens (~80 bits entropy)
- [x] Plaintext tokens never logged (SHA-256 hashes only)
- [x] /pending filtered by caller identity
- [x] Real UID lookup via nix::unistd::User
- [x] CRL file support in TLS config

### P2 — Operational Readiness ✅
- [x] tokio::process::Command for async ZFS
- [x] Install script with idempotency checks
- [x] SIGHUP config reload support
- [x] Prometheus metrics endpoint
- [x] Audit log rotation (daily)
- [x] Clean compiler warnings addressed

### P3 — NonZeroClaw & Agent Integration 🔄
- [x] Agent adapter framework
- [x] Policy engine trait defined
- [x] Signal webhook for approvals
- [ ] NZC policy engine connection (requires NZC crate integration)
- [ ] Cross-agent approval visibility (framework ready)

## Testing Checklist for .50

### P4 — Integration Testing
- [ ] Deploy to .50 with install.sh
- [ ] Test mTLS handshake with generated certs
- [ ] Verify no HTTP fallback (TLS required)
- [ ] Test ZFS snapshot with identity
- [ ] Test ZFS destroy approval flow
- [ ] Test Signal webhook (if configured)
- [ ] Test config reload (SIGHUP)
- [ ] Test metrics endpoint
- [ ] Test audit log rotation
- [ ] Verify token hashing in logs
- [ ] Verify /pending filtered by caller

## Known Limitations

1. **NZC Integration**: Framework is in place but full NonZeroClaw crate integration requires additional work to import types from `crates/nonzeroclaw`.

2. **CRL Parsing**: Basic CRL checking implemented; full ASN.1 parsing would require additional dependencies.

3. **Signal Webhook**: Requires external Signal bridge service (not included).

4. **Persistence**: Approval tokens stored in memory only; persistence would require Redis/database for multi-instance deployments.

## Next Steps

1. Complete NZC policy engine integration
2. Add systemd/PCT endpoints
3. Implement full CRL ASN.1 parsing
4. Add persistence layer for approvals
5. Web UI for approval management
6. Metrics dashboard
