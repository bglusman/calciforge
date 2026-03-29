# Native Channel Adapters for OpenClaw and NZC

## Status: **IMPLEMENTED** (v3, branch `host-agent-v3`)

The bugs described below have been fixed. Two new native adapters are now available.

---

## Background (The Original Bug)

`OpenClawHttpAdapter` dispatched all messages — including OpenClaw native
commands — via `POST /v1/chat/completions` (the LLM completions path).

**Two bug symptoms:**

1. **No conversation context** — every message was a fresh single-turn request.
   No session history was maintained across turns.

2. **Native commands broken** — commands like `/status`, `/model`, `/reasoning`,
   `!approve`, and `!deny` were **not handled natively**. They landed in the
   LLM's context as plain user messages.

`NzcHttpAdapter` correctly used the native `/webhook` endpoint but **did not
accumulate conversation history across turns** — NZC saw each message in isolation.

---

## What Was Implemented

### `openclaw-native` adapter (`src/adapters/openclaw_native.rs`)

Uses OpenClaw's **`/hooks/agent`** endpoint instead of `/v1/chat/completions`.

This runs the **full native agent loop** — same codepath as Telegram/Signal
inbound messages. OpenClaw interprets `/` and `!` tokens as native commands
before they ever reach the LLM.

**Session continuity:** Stable `sessionKey` derived from `agent_id + sender`
(format: `polyclaw:{agent_id}:{sender}`). Requires on the OpenClaw side:

```json5
{
  hooks: {
    allowRequestSessionKey: true,
    allowedSessionKeyPrefixes: ["polyclaw:"],
  }
}
```

**Config:**
```toml
[[agents]]
id = "librarian"
kind = "openclaw-native"
endpoint = "http://10.0.0.20:18789"
api_key = "REPLACE_WITH_HOOKS_TOKEN"   # hooks.token, NOT the gateway token
```

### `nzc-native` adapter (`src/adapters/nzc_native.rs`)

Wraps `NzcHttpAdapter` with a **per-sender conversation history ring buffer**.

Each request includes the prior `(user, assistant)` turns as a plain-text
preamble so NZC's agent sees the full conversational context:

```
[Conversation history]
User: <prior user message>
Assistant: <prior assistant reply>
[End history]
<current user message>
```

History is:
- Isolated per sender (`sender_key = ctx.sender || ""`)
- Capped at `MAX_HISTORY_TURNS = 20` turn-pairs (ring buffer, oldest evicted)
- Not recorded when `ApprovalPending` fires (deferred until resolution via
  `record_approval_continuation()`)

**Config:**
```toml
[[agents]]
id = "nzc"
kind = "nzc-native"
endpoint = "http://10.0.0.50:18799"
auth_token = "tok"
```

---

## Feature Comparison

| Feature                        | `openclaw-http` | `openclaw-native` | `nzc-http` | `nzc-native` |
|--------------------------------|-----------------|-------------------|------------|--------------|
| `/status`, `/model` commands   | ❌ LLM answers  | ✅ Native handler | ✅ Native  | ✅ Native    |
| `!approve` / `!deny` flows     | ❌ Not supported| ✅ Native handler | ✅ Native  | ✅ Native    |
| Session continuity             | ⚠️ Header only  | ✅ Native session | ❌ stateless | ✅ In-process history |
| History across turns           | ❌              | ✅ (OpenClaw side)| ❌         | ✅           |
| Heartbeats (HEARTBEAT_OK)      | ❌ LLM loop only| ✅ Proper heartbeat | ✅ Native | ✅ Native    |
| Tool approval escalation       | ❌ Not reachable| ✅ Full Clash flow | ✅ Native  | ✅ Native    |

---

## Backwards Compatibility

Old adapters (`openclaw-http`, `nzc-http`) are **kept unchanged** and still
registered in `build_adapter`. Existing configs continue to work as before.

---

## Tests Added

All tests pass. See test modules in the respective adapter files:

- `adapters/openclaw_native.rs` — 11 tests including:
  - `test_openclaw_native_maintains_session_across_turns`
  - `test_openclaw_native_forwards_sender_identity`
  - `test_openclaw_native_adapter_passes_session_key`

- `adapters/nzc_native.rs` — 10 tests including:
  - `test_nzc_native_appends_history`
  - `test_nzc_native_history_isolated_by_sender`
  - `test_clear_history`

- `adapters/mod.rs` — 5 new factory tests for the new kinds

---

## Tracking

- Branch: `host-agent-v3`
- Commit: `fix(polyclaw): native channel adapters for OpenClaw and NZC with session continuity`
- Files changed:
  - `src/adapters/openclaw_native.rs` (new)
  - `src/adapters/nzc_native.rs` (new)
  - `src/adapters/mod.rs` (new adapter registrations + factory entries)
  - `src/adapters/TODO-native-channel.md` (this file — updated to reflect completion)
