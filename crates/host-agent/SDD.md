# PolyClaw v3 Host-Agent — SDD Specification (Round 2)

## Document Control
- **Version**: 0.2.0
- **Date**: 2026-03-28
- **Status**: SPEC — Interface Definition Phase
- **Target**: Toy VM (.50) with real ZFS pool

---

## 1. Executive Summary

This SDD defines the security-hardened architecture for PolyClaw v3 Host-Agent integrating with NonZeroClaw (NZC) as the primary policy engine. All P0-P4 requirements from the Opus 4.6 security review are addressed.

---

## 2. Security Requirements Matrix

| ID | Requirement | Priority | Implementation Approach |
|----|-------------|----------|------------------------|
| P0-1 | Real mTLS auth middleware | P0 | Extract CN from TLS session, inject ClientIdentity into request extensions |
| P0-2 | Remove HTTP fallback | P0 | Make TLS failure fatal — no fallback server |
| P0-3 | Pass caller identity to ZFS | P0 | Use `sudo -u <identity>` for all ZFS operations |
| P0-4 | Enforce config approval rules | P0 | Check `requires_approval()` at runtime before operations |
| P1-5 | Increase token entropy | P1 | 16-char HMAC-SHA256 based tokens or 12+ char random |
| P1-6 | Remove plaintext token logging | P1 | Log only SHA-256 hash of token |
| P1-7 | Fix `/pending` endpoint | P1 | Filter by caller identity OR require admin CN |
| P1-8 | Real UID lookup | P1 | Use `nix::unistd::User::from_name()` (libc getpwnam wrapper) |
| P1-9 | Cert revocation | P1 | CRL file checking in TLS acceptor |
| P2-10 | tokio::process::Command | P2 | Replace std::process with async tokio versions |
| P2-11 | Install script | P2 | Ansible role + systemd unit template |
| P2-12 | Config reload | P2 | SIGHUP handler to reload config without restart |
| P2-13 | Prometheus metrics | P2 | `/metrics` endpoint with opentelemetry-prometheus |
| P2-14 | Audit log rotation | P2 | daily rotation with retention config |
| P2-15 | Clean dead code | P2 | Fix all compiler warnings |
| P3-16 | NZC integration | P3 | Import crates/nonzeroclaw types, route approvals through NZC |
| P3-17 | Agent adapter framework | P3 | CN→agent identity mapping, ACPX harness support |
| P3-18 | Unified approvals | P3 | NZC ApprovalPending → host-agent token → Signal webhook |
| P4-19..24 | Integration testing | P4 | Deploy to .50, test all flows |

---

## 3. Interface Specifications

### 3.1 mTLS Middleware Interface

```rust
/// Extracted from TLS session and injected into request extensions
#[derive(Debug, Clone)]
pub struct ClientIdentity {
    pub cn: String,           // Common Name from cert
    pub uid: u32,             // Resolved Unix UID
    pub username: String,     // Unix username
    pub fingerprint: String,  // Cert SHA-256 fingerprint (for revocation)
}

/// TLS acceptor that extracts client identity
pub struct MtlsAcceptor {
    inner: tokio_rustls::TlsAcceptor,
    crl: Option<CertRevocationList>,
}

impl MtlsAcceptor {
    /// Accept connection, verify cert not revoked, extract CN
    pub async fn accept(&self, stream: TcpStream) -> Result<(ClientIdentity, TlsStream)>;
}
```

### 3.2 NZC Policy Interface

```rust
/// Host-agent operation types that NZC can evaluate
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HostOperation {
    ZfsSnapshot { dataset: String, snapname: String },
    ZfsDestroy { target: String },
    ZfsList { dataset: Option<String> },
    PctStart { vmid: u32 },
    PctStop { vmid: u32 },
    SystemdRestart { unit: String },
}

/// Policy evaluation request
pub struct PolicyRequest {
    pub caller: ClientIdentity,
    pub operation: HostOperation,
    pub context: serde_json::Value, // timestamps, previous ops, etc.
}

/// Policy evaluation response from NZC
pub enum PolicyDecision {
    Allow,
    Deny { reason: String },
    ApprovalRequired { 
        nzc_request_id: String,
        host_token: String,  // 16-char token for Signal confirmation
    },
}

/// NZC integration trait
pub trait NzcPolicyEngine: Send + Sync {
    async fn evaluate(&self, request: PolicyRequest) -> Result<PolicyDecision>;
    async fn check_approval(&self, nzc_request_id: &str) -> Result<ApprovalStatus>;
}
```

### 3.3 Agent Adapter Interface

```rust
/// Supported agent types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentType {
    Librarian,      // Brian's primary agent
    Lucien,         // Infrastructure guardian
    Zeroclaw,       // NZC CLI agent
    AcpHarness,     // ACPX agents (Codex, Claude Code, etc.)
    Custom(&'static str),
}

/// Adapter for different agent identity formats
pub trait AgentAdapter: Send + Sync {
    /// Extract agent identity from certificate CN
    fn identify(&self, cn: &str) -> Option<AgentIdentity>;
    
    /// Map agent to Unix user for ZFS delegation
    fn resolve_unix_user(&self, identity: &AgentIdentity) -> Option<(String, u32)>;
    
    /// Get NZC policy profile for this agent
    fn policy_profile(&self, identity: &AgentIdentity) -> PolicyProfile;
}

pub struct AgentIdentity {
    pub agent_type: AgentType,
    pub instance: String,     // e.g., "main", "coding-session-abc"
    pub cert_cn: String,      // Original CN
}

pub struct PolicyProfile {
    pub autonomy_level: AutonomyLevel,  // from NZC
    pub allowed_operations: Vec<String>,
    pub requires_approval_for: Vec<String>,
}
```

### 3.4 Signal Webhook Interface

```rust
/// Signal webhook payload for approval confirmations
#[derive(Debug, Deserialize)]
pub struct SignalWebhookPayload {
    pub token: String,           // 16-char approval token
    pub confirmation_code: String, // "CONFIRM" or similar
    pub approver: String,        // Signal number that confirmed
    pub timestamp: DateTime<Utc>,
}

/// Signal integration for human-in-the-loop
pub struct SignalApprover {
    webhook_url: String,
    http_client: reqwest::Client,
}

impl SignalApprover {
    /// Send approval request via Signal
    pub async fn notify(&self, token: &str, operation: &HostOperation) -> Result<()>;
    
    /// Validate webhook callback
    pub fn validate_callback(&self, payload: &SignalWebhookPayload) -> Result<ApprovalValidation>;
}
```

---

## 4. Component Architecture

### 4.1 Layer Diagram

```
┌─────────────────────────────────────────────────────────────────┐
│                        Client Agents                            │
│  (Librarian, Lucien, Zeroclaw, ACPX Codex/Claude Code)          │
└─────────────────────────────────────────────────────────────────┘
                              │ mTLS
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                    mTLS Termination Layer                       │
│  • Certificate validation                                       │
│  • CRL checking (P1-9)                                          │
│  • CN extraction                                                │
│  • ClientIdentity injection                                     │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                   Agent Adapter Layer                           │
│  • CN → AgentIdentity resolution (P3-17)                        │
│  • Agent → Unix user mapping (P1-8)                             │
│  • Policy profile selection                                     │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                  NonZeroClaw Policy Engine                      │
│  • Policy evaluation (P3-16)                                    │
│  • ApprovalRequired → token generation                          │
│  • Cross-agent approval visibility (P3-18)                      │
└─────────────────────────────────────────────────────────────────┘
                              │
                    ┌─────────┴─────────┐
                    │                   │
            Allow / Deny          ApprovalRequired
                    │                   │
                    ▼                   ▼
┌─────────────────────────┐  ┌──────────────────────────────┐
│    Operation Executor   │  │   Signal Webhook Handler     │
│  • ZFS (P2-10 async)    │  │  • Token validation          │
│  • PCT                  │  │  • Human confirmation          │
│  • systemd              │  │  • NZC approval update         │
└─────────────────────────┘  └──────────────────────────────┘
         │                              │
         ▼                              ▼
┌─────────────────────────┐  ┌──────────────────────────────┐
│    Audit Logger         │  │   Unified Approval Store     │
│  • JSONL output         │  │  • In-memory + persistence   │
│  • Daily rotation       │  │  • Cross-agent visibility    │
│  (P2-14)                │  │  (P3-18)                     │
└─────────────────────────┘  └──────────────────────────────┘
```

### 4.2 Data Flow: Destructive Operation with Approval

```
1. Client sends: POST /zfs/destroy {dataset: "tank/media@old", approval_token: null}

2. mTLS layer extracts CN="librarian" → ClientIdentity

3. AgentAdapter resolves: librarian → (uid=1000, user="librarian")

4. NZC PolicyEngine evaluates:
   - Operation: ZfsDestroy {target: "tank/media@old"}
   - Caller: librarian
   - Decision: ApprovalRequired {nzc_request_id: "...", host_token: "X7K9M2P4Q8R5N6V3"}

5. Host-agent:
   - Creates approval record with 16-char token
   - Logs SHA-256 hash only (P1-6)
   - Calls Signal webhook to notify Brian
   - Returns: {pending_approval: true, token: "X7K9M2P4Q8R5N6V3"}

6. Brian receives Signal: "Librarian wants to destroy tank/media@old. Reply CONFIRM X7K9M2P4Q8R5N6V3"

7. Brian replies CONFIRM → Signal webhook → Host-agent

8. Host-agent validates token, marks NZC request approved

9. ZFS operation executes with sudo -u librarian (P0-3)

10. Result logged to audit.jsonl
```

---

## 5. File Structure

```
crates/host-agent/src/
├── main.rs              # Entry point, server setup
├── lib.rs               # Public exports (for testing)
├── config.rs            # Config loading, SIGHUP reload (P2-12)
├── error.rs             # Error types
├── audit.rs             # Audit logging with rotation (P2-14)
├── metrics.rs           # Prometheus metrics (P2-13)
│
├── tls/
│   ├── mod.rs           # TLS config
│   ├── acceptor.rs      # MtlsAcceptor with CRL (P1-9)
│   └── middleware.rs    # ClientIdentity extraction (P0-1)
│
├── auth/
│   ├── mod.rs           # ClientIdentity, auth middleware
│   ├── identity.rs      # CN → UID resolution (P1-8)
│   └── adapter.rs       # Agent adapter framework (P3-17)
│
├── policy/
│   ├── mod.rs           # Policy engine trait
│   ├── nzc.rs           # NonZeroClaw integration (P3-16)
│   └── rules.rs         # Runtime rule checking (P0-4)
│
├── approval/
│   ├── mod.rs           # ApprovalManager
│   ├── token.rs         # 16-char HMAC token gen (P1-5, P1-6)
│   ├── signal.rs        # Signal webhook (P3-18)
│   └── store.rs         # Persistent approval store
│
├── zfs/
│   ├── mod.rs           # ZFS operations
│   ├── executor.rs      # Async ZFS commands (P2-10)
│   └── validation.rs    # Dataset name validation
│
└── handlers/
    ├── mod.rs           # Axum route handlers
    ├── health.rs        # Health check + metrics
    ├── zfs.rs           # ZFS endpoints
    └── approval.rs      # Approval endpoints
```

---

## 6. Configuration Schema

```toml
[server]
bind = "0.0.0.0:18443"
cert = "/etc/clash/certs/server.crt"
key = "/etc/clash/certs/server.key"
client_ca = "/etc/clash/certs/ca.crt"
crl_file = "/etc/clash/certs/ca.crl"  # Optional (P1-9)

[audit]
log_path = "/var/log/clash/audit.jsonl"
rotation = "daily"  # daily, hourly, or never (P2-14)
retention_days = 90

[approval]
ttl_seconds = 300
token_entropy_bits = 80  # 16 chars = ~80 bits (P1-5)
signal_webhook = "https://signal.example.com/webhook"
require_confirmation = true

[metrics]
enabled = true
bind = "127.0.0.1:19090"  # Prometheus endpoint (P2-13)

[[agent]]
cn_pattern = "librarian*"
agent_type = "librarian"
unix_user = "librarian"
autonomy = "supervised"  # readonly, supervised, full

[[agent]]
cn_pattern = "claude-code*"
agent_type = "acp_harness"
unix_user = "clash-agent"
autonomy = "supervised"
allowed_operations = ["zfs-list", "zfs-snapshot"]

[[rule]]
operation = "zfs-destroy"
approval_required = true
pattern = "tank/.*"  # Regex for dataset matching (P0-4)

[[rule]]
operation = "zfs-destroy"
approval_required = false
pattern = "tank/temp/.*"  # temp datasets don't need approval
```

---

## 7. Deployment Specification

### 7.1 Ansible Role Structure

```yaml
# roles/host-agent/tasks/main.yml
- name: Install clash-host-agent binary
  copy:
    src: clash-host-agent
    dest: /usr/local/bin/clash-host-agent
    mode: '0755'

- name: Create service user
  user:
    name: clash-agent
    system: yes
    shell: /bin/false

- name: Setup ZFS delegation
  command: "zfs allow -u {{ item.user }} {{ item.perms }} {{ item.dataset }}"
  loop: "{{ zfs_delegations }}"

- name: Configure sudoers for ZFS destroy
  template:
    src: sudoers.j2
    dest: /etc/sudoers.d/clash-agent
    validate: 'visudo -cf %s'

- name: Generate mTLS certificates
  command: "{{ item }}"
  loop:
    - "openssl genrsa -out ca.key 4096"
    - "openssl req -new -x509 -key ca.key -sha256 -subj '/C=US/O=PolyClaw/CN=PolyClaw CA' -days 3650 -out ca.crt"
    # ... server and client certs

- name: Install systemd unit
  template:
    src: host-agent.service.j2
    dest: /etc/systemd/system/clash-host-agent.service
  notify: reload systemd

- name: Start and enable service
  systemd:
    name: clash-host-agent
    state: started
    enabled: yes
```

### 7.2 Systemd Unit Template

```ini
[Unit]
Description=PolyClaw Host-Agent mTLS RPC Server
After=network.target

[Service]
Type=notify
ExecStart=/usr/local/bin/clash-host-agent --config /etc/clash/host-agent.toml
ExecReload=/bin/kill -HUP $MAINPID
Restart=on-failure
RestartSec=5
User=clash-agent
Group=clash-agent

# Security hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/log/clash
AmbientCapabilities=CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
```

---

## 8. Testing Plan (P4)

### 8.1 Test Matrix

| Test | Description | Success Criteria |
|------|-------------|------------------|
| T1 | mTLS handshake | Client with valid cert connects, invalid rejected |
| T2 | CRL revocation | Revoked cert cannot connect (P1-9) |
| T3 | HTTP fallback removed | TLS failure = fatal, no plaintext server (P0-2) |
| T4 | CN extraction | Correct identity extracted from various CNs |
| T5 | UID resolution | librarian→1000, unknown→error (P1-8) |
| T6 | ZFS snapshot | Creates snapshot as calling user (P0-3) |
| T7 | ZFS destroy approval | Pending state, Signal notify, confirm, execute |
| T8 | Token entropy | 16-char tokens, no collisions in 10k samples (P1-5) |
| T9 | Token hash logging | Logs show SHA-256, not plaintext (P1-6) |
| T10 | Config reload | SIGHUP reloads without restart (P2-12) |
| T11 | Metrics endpoint | /metrics returns valid Prometheus format (P2-13) |
| T12 | Audit rotation | Daily rotation, old files cleaned (P2-14) |
| T13 | NZC policy | Librarian allowed, unknown agent denied |
| T14 | Cross-agent visibility | Approval from Lucien visible to Librarian (P3-18) |
| T15 | Install idempotency | Run twice = no changes, service running (P2-11, P4-24) |

---

## 9. Glossary

- **NZC**: NonZeroClaw — the primary agent/policy engine
- **mTLS**: Mutual TLS — both client and server authenticate with certificates
- **CN**: Common Name — X.509 certificate field identifying the client
- **CRL**: Certificate Revocation List — list of revoked certificates
- **ACPX**: Anthropic Computer Protocol eXtended — agent harness protocol

---

## 10. Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 0.1.0 | 2026-03-28 | SDD Round 1 | Initial architecture |
| 0.2.0 | 2026-03-28 | SDD Round 2 | NZC integration, security fixes |
