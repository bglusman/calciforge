
---

## Channel-Level Outpost / Content Interception Layer
*Captured: 2026-03-31 — Brian*

**Idea:** Extend the outpost/scanning layer to optionally man-in-the-middle all channel communications — not just tool results, but inbound messages from users as well.

**Motivation:**
- Group chats: untrusted participants can send injection payloads; scanning inbound before routing is valuable
- DMs: useful if untrusted users have physical access to devices (phone/computer/iPad) and can craft messages
- Closes the gap between "scan what the LLM sees from tools" and "scan what the LLM sees from all inputs"

**Proposed placement:** NonZeroClawed ingress pipeline, between channel receive and identity/router dispatch. Every message would pass through a configurable scan policy before reaching the agent.

**Naming:** "Outpost" is overloaded (OpenClaw tool result scanner + outpost-lite service). This feature deserves a new name. Candidates:
- **Censor** — Brian's suggestion; accurate, clear, slightly heavy-handed connotation
- **Sentry** — watches the gate, doesn't necessarily block
- **Checkpoint** — neutral, process-oriented
- **Gatekeeper** — strong, clear role
- **Filter** — generic but unambiguous

**Scope options:**
- v1: Scan-and-log only (observe without blocking)
- v2: Configurable per-channel: scan-only / flag-and-deliver / block-and-reject
- v3: Per-identity trust levels (owner = passthrough, unknown = full scan, group = configurable)

**Relationship to existing components:**
- outpost-lite (10.0.0.20:9877): injection detection service — could reuse its verdict API
- clash policy: could gate on scan verdict ("block if UNSAFE, flag if REVIEW, passthrough if CLEAN")
- TTSR stream-rewrite: complementary — inbound scan catches *input* injection, TTSR catches *output* drift

**Open questions:**
- Does scanning inbound content from trusted identities (Brian) add value or just latency?
- Should scanned-and-modified messages be flagged to the user transparently?
- Privacy: scanning DM content means the scanner sees everything — audit logging must be opt-in

**Priority:** Low/research. Capture in v3 roadmap. Revisit after NonZeroClawed v3 core (policy + approval plumbing) is stable.
