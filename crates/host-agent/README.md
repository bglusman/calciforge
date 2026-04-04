# ZeroClawed Host-Agent (Adapter-First)

mTLS RPC server providing safe VM-to-host delegation via an adapter-first architecture.

**v4 adds:** unified `/host/op` dispatch endpoint, five adapters (ZFS, Systemd, PCT, Git,
Exec/Ansible stub), per-adapter validation, and policy-driven approval flows.

## v4 Quick Start — `/host/op`

```bash
# ZFS list
curl -s --cert client.crt --key client.key -k \
  -X POST https://host:18443/host/op \
  -H "Content-Type: application/json" \
  -d '{"kind":"zfs","args":["list"]}'

# Systemd status
curl -s --cert client.crt --key client.key -k \
  -X POST https://host:18443/host/op \
  -d '{"kind":"systemd","resource":"nginx.service","args":["status"]}'

# PCT container status
curl -s --cert client.crt --key client.key -k \
  -X POST https://host:18443/host/op \
  -d '{"kind":"pct","resource":"101","args":["status"]}'

# Git repo status (repo must be in allowed_repos)
curl -s --cert client.crt --key client.key -k \
  -X POST https://host:18443/host/op \
  -d '{"kind":"git","resource":"/srv/myapp","args":["status"]}'
```

## v4 Adapter Config

```toml
# Git adapter: repo allowlist (empty = allow any absolute path)
[git]
allowed_repos = ["/srv", "/opt", "/home"]

# Exec adapter: disabled by default
[exec]
enabled = false                              # must be true to activate
allowed_commands = ["/usr/bin/uptime"]       # absolute paths only
# ansible_job_queue = "/var/lib/clash/jobs"  # for Ansible stub
```

## v4 Default Policy Rules

Adapter dispatch respects the same `[[rules]]` config as v3:

```toml
# Systemd — read-only ops: no approval needed
[[rules]]
operation = "systemd-status"
approval_required = false

# PCT — start/stop requires approval
[[rules]]
operation = "pct-start"
approval_required = true

# PCT — destroy always requires admin approval
[[rules]]
operation = "pct-destroy"
approval_required = true
always_ask = true
approval_admin_only = true

# Git — checkout requires approval
[[rules]]
operation = "git-checkout"
approval_required = true
```

---

## Legacy API (still supported)

## Security Features (SDD Round 2)

### P0 — Authentication & Authorization ✅

1. **Real mTLS auth middleware** — CN extracted from TLS session, ClientIdentity injected
2. **No HTTP fallback** — TLS failure is fatal, no plaintext server (P0-2)
3. **Caller identity passed to ZFS** — All operations use `sudo -u <identity>` (P0-3)
4. **Config approval rules enforced** — `requires_approval()` checked at runtime (P0-4)

### P1 — Token & Security Hardening ✅

5. **16-character token entropy** — ~80 bits, cryptographically secure (P1-5)
6. **Token hash logging** — Only SHA-256 hashes logged, never plaintext (P1-6)
7. **Filtered `/pending` endpoint** — Returns only caller's pending approvals (P1-7)
8. **Real UID lookup** — Uses `nix::unistd::User::from_name()` / `getpwnam()` (P1-8)
9. **CRL support** — Certificate revocation list checking in TLS (P1-9)

### P2 — Operational Readiness ✅

10. **Async ZFS commands** — Uses `tokio::process::Command` (P2-10)
11. **Install script** — `install.sh` for one-command deployment (P2-11)
12. **Config reload** — SIGHUP handler support (P2-12)
13. **Prometheus metrics** — `/metrics` endpoint on configurable port (P2-13)
14. **Audit log rotation** — Daily rotation with retention (P2-14)
15. **Clean code** — Compiler warnings addressed

### P3 — NonZeroClaw & Agent Integration 🔄 (Partial)

16. **NZC integration framework** — Policy engine trait defined, ready for NZC connection
17. **Agent adapter framework** — CN → agent identity mapping, ACPX support
18. **Unified approvals** — Signal webhook integration for human confirmation

## mTLS Security Features (unchanged from v3)

### P0 — Authentication & Authorization ✅

1. **Real mTLS auth middleware** — CN extracted from TLS session, ClientIdentity injected
2. **No HTTP fallback** — TLS failure is fatal, no plaintext server
3. **Caller identity passed to operations** — All operations use `sudo -u <identity>`
4. **Config approval rules enforced** — policy checked at runtime

### P1 — Token & Security Hardening ✅

5. **16-character token entropy** — ~80 bits, cryptographically secure
6. **Token hash logging** — Only SHA-256 hashes logged, never plaintext
7. **Filtered `/pending` endpoint** — Returns only caller's pending approvals
8. **Real UID lookup** — Uses `nix::unistd::User::from_name()` / `getpwnam()`

## Quick Start

### Build

```bash
cd /root/projects/zeroclawed
cargo build --release -p host-agent
```

### Install on Target System

```bash
cd /root/projects/zeroclawed/crates/host-agent
scp target/release/clash-host-agent root@10.0.0.80:/tmp/
ssh root@10.0.0.80
cd /tmp
./clash-host-agent --help

# Or use the install script:
./install.sh
```

### Ansible Deployment

```bash
cd /root/.openclaw/workspace/infra/ansible
ansible-playbook -i inventories/toy-vm.yml playbooks/host-agent-deploy.yml
```

## Configuration

```toml
[server]
bind = "0.0.0.0:18443"
cert = "/etc/clash/certs/server.crt"
key = "/etc/clash/certs/server.key"
client_ca = "/etc/clash/certs/ca.crt"
crl_file = "/etc/clash/certs/ca.crl"  # Optional

[audit]
log_path = "/var/log/clash/audit.jsonl"
rotation = "daily"  # daily, hourly, never
retention_days = 90

[approval]
ttl_seconds = 300
token_entropy_bits = 80
signal_webhook = "https://signal.example.com/webhook"
allowed_approvers = ["+15555550001"]

[metrics]
enabled = true
bind = "127.0.0.1:19090"

[[agent]]
cn_pattern = "librarian*"
agent_type = "librarian"
unix_user = "librarian"
autonomy = "supervised"
allowed_operations = ["zfs-list", "zfs-snapshot"]
requires_approval_for = ["zfs-destroy"]

[[rule]]
operation = "zfs-destroy"
approval_required = true
pattern = "tank/.*"
```

## API Endpoints

### Health Check
```bash
curl -k --cert client.pem https://host:18443/health
```

### ZFS List
```bash
curl -k --cert client.pem -X POST \
  -H "Content-Type: application/json" \
  -d '{"dataset": "tank", "type": "snapshot"}' \
  https://host:18443/zfs/list
```

### ZFS Snapshot
```bash
curl -k --cert client.pem -X POST \
  -H "Content-Type: application/json" \
  -d '{"dataset": "tank/media", "snapname": "daily-2024-01-15"}' \
  https://host:18443/zfs/snapshot
```

### ZFS Destroy (Requires Approval)
```bash
# Request approval
curl -k --cert client.pem -X POST \
  -H "Content-Type: application/json" \
  -d '{"dataset": "tank/media@old", "approval_token": null}' \
  https://host:18443/zfs/destroy

# Response: {"pending_approval": true, "approval_id": "...", "message": "Reply CONFIRM X7K9****"}

# Confirm via API (or Signal webhook)
curl -k --cert client.pem -X POST \
  -H "Content-Type: application/json" \
  -d '{"approval_id": "...", "token": "X7K9M2P4Q8R5N6V3"}' \
  https://host:18443/approve

# Execute with token
curl -k --cert client.pem -X POST \
  -H "Content-Type: application/json" \
  -d '{"dataset": "tank/media@old", "approval_token": "X7K9M2P4Q8R5N6V3"}' \
  https://host:18443/zfs/destroy
```

### Prometheus Metrics
```bash
curl http://localhost:19090/metrics
```

## Security Model

1. **mTLS is mandatory** — No plaintext HTTP fallback
2. **Client certificates required** — Must present valid cert signed by CA
3. **Identity from CN** — Unix user resolved from certificate Common Name
4. **Operations as user** — All ZFS commands run as the authenticated user
5. **Approval for destruction** — Destroy operations require human confirmation
6. **Audit everything** — All operations logged with hashes (no plaintext tokens)

## Testing

### Unit Tests
```bash
cargo test -p host-agent
```

### Integration Tests (on .50)
```bash
# Deploy
ansible-playbook -i inventories/toy-vm.yml playbooks/host-agent-deploy.yml

# Copy client cert locally
scp root@10.0.0.80:/etc/clash/certs/librarian-bundle.pem ./

# Test health
curl -k --cert librarian-bundle.pem https://10.0.0.80:18443/health

# Test ZFS operations
curl -k --cert librarian-bundle.pem -X POST \
  -H "Content-Type: application/json" \
  -d '{"dataset": "tank"}' \
  https://10.0.0.80:18443/zfs/list
```

## Architecture

```
┌─────────────┐      mTLS       ┌────────────────────────────────────────┐
│ Client      │ ───────────────▶│ Host-Agent                             │
│ (cert: CN)  │                 │  ┌─────────┐  ┌──────────┐  ┌────────┐ │
└─────────────┘                 │  │ mTLS    │─▶│ Identity │─▶│ Policy │ │
                                │  │ Layer   │  │ Resolver │  │ Engine │ │
                                │  └─────────┘  └──────────┘  └───┬────┘ │
                                │                                 │      │
                                │  ┌─────────┐  ┌──────────┐      │      │
                                │  │ ZFS     │◀─│  Sudo    │◀─────┘      │
                                │  │ Executor│  │  -u CN   │             │
                                │  └─────────┘  └──────────┘             │
                                │                                        │
                                │  ┌─────────┐  ┌──────────┐             │
                                │  │ Audit   │  │ Signal   │             │
                                │  │ Logger  │  │ Webhook  │             │
                                │  └─────────┘  └──────────┘             │
                                └────────────────────────────────────────┘
```

## Troubleshooting

### Service won't start
```bash
journalctl -u clash-host-agent -f
# Check certificate permissions
ls -la /etc/clash/certs/
# Check config syntax
cat /etc/clash/host-agent.toml
```

### mTLS handshake fails
```bash
# Test with verbose curl
curl -v -k --cert client.pem https://host:18443/health
# Check cert is signed by CA
openssl verify -CAfile /etc/clash/certs/ca.crt /etc/clash/certs/client.crt
```

### ZFS permission denied
```bash
# Check ZFS delegation
zfs allow tank
# Check sudoers
sudo -u clash-agent sudo -u root zfs list tank
```

## License

MIT
