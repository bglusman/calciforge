# Approval Relay Design Sketch

_Research date: 2026-03-30_

---

## The Problem

Bitwarden's Agent Access SDK approval flow is interactive CLI:
```
[Agent] → aac connect → [proxy] → aac listen (interactive terminal) → user types "yes"
```

This is synchronous, terminal-dependent, and completely wrong for async agent workflows over Signal/Telegram.

**What we need**: agent requests a credential → ZeroClawed/NZC routes the approval request to the user as a chat message → user taps "Approve" → credential is released.

---

## System Components

```
┌──────────────────────────────────────────────────────────────────────┐
│                           ZeroClawed / NZC                             │
│                                                                       │
│  ┌─────────────┐    ┌──────────────────┐    ┌────────────────────┐  │
│  │  Channel    │    │  Approval Relay   │    │   Vault Adapter    │  │
│  │  (Signal/   │◄──►│  (state machine)  │◄──►│  (Bitwarden/VW/   │  │
│  │   Telegram) │    │                  │    │   OneCLI/Env)      │  │
│  └─────────────┘    └──────────────────┘    └────────────────────┘  │
│                              │                                        │
│                      ┌───────▼───────┐                               │
│                       │  Pending      │                               │
│                       │  Approvals    │                               │
│                       │  (in-memory   │                               │
│                       │  + persisted) │                               │
│                       └───────────────┘                              │
└──────────────────────────────────────────────────────────────────────┘
         ▲                                        ▲
         │                                        │
    User Chat                               Downstream Agent
    (Signal/Telegram)                       (OpenClaw or NZC)
```

---

## Full Approval Flow

### Step 1: Agent Requests Credential

The downstream agent (OpenClaw or NZC agent) makes a vault call:
```
vault_adapter.request_approval("github.com", "git push to main branch")
```

Or if using OneCLI as the proxy layer — the agent's HTTP call through the OneCLI gateway gets intercepted because the credential requires approval.

### Step 2: Approval Relay Creates Pending Request

ZeroClawed's Approval Relay:
1. Generates a short-lived approval token (UUID, e.g. `apr_abc123`)
2. Stores state:
   ```json
   {
     "id": "apr_abc123",
     "created_at": 1774893094,
     "expires_at": 1774893394,     // 5-minute TTL
     "credential_key": "github.com",
     "context": "git push to main branch",
     "agent_id": "main",
     "requester": "telegram:+15551234567",  // who should approve
     "status": "pending"
   }
   ```
3. Suspends the agent's turn (or signals the credential will be async)

### Step 3: Route Approval Request to User

ZeroClawed sends a message to the user's configured approval channel (Signal, Telegram, etc.):

```
🔐 Credential Request

Agent "Librarian" is requesting access to:
  github.com (git push to main branch)

Expires in: 5 minutes
Request ID: apr_abc123

Reply: /approve apr_abc123  or  /deny apr_abc123
```

Or, for richer platforms (Telegram with inline buttons):
```
🔐 Credential Request

Librarian wants to access github.com
Context: git push to main branch

[✅ Approve]  [❌ Deny]
```

### Step 4: User Approves (or Denies)

**Path A — Text command**:
```
User: /approve apr_abc123
ZeroClawed: ✅ Approved. Credential released.
```

**Path B — Inline button (Telegram)**:
```
User: [taps ✅ Approve]
ZeroClawed: ✅ Approved. Credential released.
```

**Path C — Timeout**:
```
[5 minutes elapse — no response]
ZeroClawed: ⏱️ Approval request apr_abc123 expired. Credential not released.
→ Agent receives: ApprovalError::Expired
```

### Step 5: Credential Released

After approval:
1. Approval Relay updates state: `status: "approved"`
2. Vault Adapter retrieves the actual credential (e.g. calls `bw` CLI or Bitwarden SDK, or reads from Vaultwarden)
3. Returns `Secret` to the waiting agent call
4. Agent resumes its work

---

## State Machine

```
           ┌──────────┐
           │ PENDING  │ ← created, message sent to user
           └─────┬────┘
                 │
        ┌────────┼────────┐
        ▼        ▼        ▼
   ┌────────┐ ┌──────┐ ┌─────────┐
   │APPROVED│ │DENIED│ │ EXPIRED │
   └────────┘ └──────┘ └─────────┘
```

Transitions:
- PENDING → APPROVED: user sends /approve or taps button
- PENDING → DENIED: user sends /deny or taps button
- PENDING → EXPIRED: TTL elapsed without response
- APPROVED → (terminal): credential delivered, token consumed
- DENIED / EXPIRED → (terminal): agent receives error

---

## Rust Interface Design

```rust
/// Represents a pending approval request
pub struct ApprovalRequest {
    pub id: ApprovalId,           // e.g. "apr_abc123"
    pub credential_key: String,   // e.g. "github.com"
    pub context: String,          // human-readable purpose
    pub created_at: SystemTime,
    pub expires_at: SystemTime,
    pub approver_channel: ChannelId,  // where to send the request
    pub approver_peer: PeerId,        // who to send it to
}

pub enum ApprovalOutcome {
    Approved(ApprovalToken),
    Denied { reason: Option<String> },
    Expired,
}

/// The relay trait — ZeroClawed implements this
#[async_trait]
pub trait ApprovalRelay: Send + Sync {
    /// Send approval request to user, return outcome (blocking until resolved or expired)
    async fn request_approval(
        &self,
        req: ApprovalRequest,
    ) -> Result<ApprovalOutcome>;
    
    /// Handle an incoming approval response from a channel message
    async fn handle_response(
        &self,
        approval_id: &ApprovalId,
        response: ApprovalResponse,
    ) -> Result<()>;
    
    /// List pending approvals (for status/monitoring)
    async fn list_pending(&self) -> Vec<ApprovalRequest>;
}

pub enum ApprovalResponse {
    Approve,
    Deny { reason: Option<String> },
}
```

---

## Approval Granularity Options

The planning doc asks: per-use vs per-session vs trust-until-revoked?

### Option A: Per-Use (strictest)
Every single credential access requires explicit approval. 

- Pro: maximum control, full audit trail
- Con: extremely annoying for agents that call the same API repeatedly
- Use case: initial setup, high-risk credentials (financial APIs, admin keys)

### Option B: Per-Session
Approve once → agent can use that credential for the duration of the current session.

- Pro: good balance of security and usability
- Con: what's a "session"? Must define session boundaries clearly
- Use case: normal agent work sessions

### Option C: Trust-Until-Revoked (least friction)
Approve once → agent can use indefinitely until user explicitly revokes.

- Pro: seamless agent operation
- Con: weakest security guarantee, easy to forget what's approved
- Use case: development/trusted agent setup

### Option D: Time-Bound
Approve for N hours/days (configurable per credential).

- Pro: best security/usability tradeoff
- Con: slightly more complex UX (user must confirm duration)
- Use case: **recommended default**

**Recommendation**: Default to **per-session** (A for initial testing, B for production). Add time-bound (D) as an advanced option. Trust-until-revoked only for credentials explicitly marked low-risk.

---

## Multi-Approval UX Considerations

### Batch Approvals
If an agent needs multiple credentials quickly (e.g. SSH key + API key), send a batch:

```
🔐 Credential Requests (2 pending)

1. github.com — git push to main
2. openai.com — API call

[✅ Approve All]  [❌ Deny All]  [Review Individually]
```

### Approval Context Quality
The `context` field is critical. A bare "github.com" is not helpful. The agent should provide rich context:

```
// Bad
vault_adapter.request_approval("github.com", "API call")

// Good  
vault_adapter.request_approval("github.com", "git push: 3 files changed in zeroclawed/src/")
```

ZeroClawed should validate that context is non-empty and meaningful before forwarding the request.

### Approval Channel vs Agent Channel
The approval should go to the **operator/owner**, not to whoever the agent is currently talking to.

- Brian uses Librarian via Telegram and Signal
- When Librarian needs a credential, the approval goes to Brian's configured admin channel
- Not to a random user who might be in a group chat with Librarian

Config: `zeroclawed.approval.channel = "telegram"` + `zeroclawed.approval.to = "+15551234567"`

---

## Integration with OneCLI

OneCLI (as a sidecar) handles **injection** but not async approval. The integration point:

```
[Agent HTTP call] → [OneCLI gateway intercepts]
                          │
                    Needs approval?
                    ├── NO → inject credential, forward
                    └── YES → ???
                              │
                    Option A: OneCLI returns 403 + approval-required metadata
                              → NZC catches 403, triggers approval relay
                              → On approval, agent retries the call
                              
                    Option B: OneCLI calls NZC webhook to request approval
                              → NZC relays to user
                              → On approval, NZC calls back to OneCLI to unblock
```

**Option A is simpler** and doesn't require OneCLI to know about NZC. Agent retries are easy to implement.

**Option B is cleaner** (no retry storms if approval takes a long time) but requires OneCLI to support a callback URL — this is on their roadmap but not available yet.

**For Phase 1**: implement Option A. Agent catches a credential-unavailable error, triggers the approval relay, waits for approval token, retries.

---

## Wire Protocol (NZC ↔ ZeroClawed Approval Relay)

If NZC and ZeroClawed are separate processes, they need a protocol:

```rust
// NZC → ZeroClawed: request approval
POST http://zeroclawed-relay/approvals/request
{
  "agent_id": "main",
  "credential_key": "github.com",
  "context": "git push: 3 files changed",
  "ttl_seconds": 300
}
→ { "approval_id": "apr_abc123", "status": "pending" }

// NZC polls (or ZeroClawed webhooks back to NZC)
GET http://zeroclawed-relay/approvals/apr_abc123
→ { "status": "pending" | "approved" | "denied" | "expired" }

// On approval: NZC retrieves credential
POST http://zeroclawed-relay/approvals/apr_abc123/claim
→ { "status": "approved", "credential": { ... } }  // credential delivered once
```

Or for in-process integration (ZeroClawed embedded in NZC):
- Use a `tokio::sync::oneshot::channel` per approval
- Approval relay holds the sender; agent task holds the receiver
- When user approves → send(ApprovalOutcome::Approved(token))
- Agent resumes

---

## Security Considerations

1. **Approval tokens are single-use**: once claimed, the `approved` state cannot be replayed
2. **TTL enforcement**: expired tokens must be rejected even if user tries to approve after expiry
3. **Channel validation**: only the configured approver channel/peer can approve — a random Signal message can't approve someone else's request
4. **Credential isolation**: the approval token does NOT contain the credential — it's an authorization ticket. The vault fetch happens separately after approval is confirmed.
5. **Replay prevention**: approval IDs are UUIDs with timestamps; same ID can't be reused
6. **Audit log**: every approval/denial/expiry is logged with timestamp, approver identity, and context

---

## Open Questions

1. **What happens if the agent times out waiting for approval?** → Must return a clear error so the agent can tell the user "waiting for credential approval" rather than hanging silently.

2. **Should NZC wake a sleeping OpenClaw agent when approval arrives?** → Probably via `/hooks/wake` endpoint.

3. **Where are pending approvals persisted?** → In-memory is fine for simple cases; for reliability, persist to a lightweight DB or JSONL file so restarts don't lose pending requests.

4. **Multi-approver?** → Not needed for Phase 1. Single operator per NZC instance.
