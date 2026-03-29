# TODO: Native Channel Plugin for OpenClaw

## The Problem

`OpenClawHttpAdapter` dispatches all messages — including OpenClaw native
commands — via `POST /v1/chat/completions` (the LLM completions path).

This means commands like `/status`, `/model`, `/reasoning`, `!approve`, and
`!deny` are **not handled natively**. Instead they land in the LLM's context as
plain user messages, which produces:

- Confusing or incorrect responses (the LLM tries to answer "what is /status?")
- No ability to trigger real approval flows from PolyClaw
- No heartbeat integration, no reaction support, no session commands

## The Root Cause

OpenClaw's native command handling lives in its **inbound message pipeline**
(the channel plugin layer). The `/v1/chat/completions` HTTP shim completely
bypasses that pipeline — it's a read-only LLM proxy that has no knowledge of
OpenClaw sessions, commands, or reactions.

## The Solution

Implement a **`PolyClawChannelPlugin`** for OpenClaw that feeds messages
directly into OpenClaw's native inbound message pipeline, exactly as if
PolyClaw were a first-class channel (like Telegram or Signal).

### What this requires

1. **OpenClaw side** — expose a `/v1/inbound` (or equivalent) endpoint that
   accepts a raw message envelope `{text, sender, channel}` and runs it through
   the full native pipeline (command dispatch → tool calls → memory → response).

2. **PolyClaw side** — add a new adapter kind (e.g. `kind: "openclaw-native"`)
   that calls `/v1/inbound` instead of `/v1/chat/completions`. This adapter
   would be registered via the existing `build_adapter` factory in
   `adapters/mod.rs`.

3. **Config** — update `polyclaw.yaml` agent entries to use
   `kind: openclaw-native` for agents that need native command support.

### What it unlocks

| Feature                        | HTTP adapter (`openclaw-http`) | Native channel plugin |
|--------------------------------|--------------------------------|----------------------|
| `/status`, `/model` commands   | ❌ LLM answers instead        | ✅ Native handler    |
| `!approve` / `!deny` flows     | ❌ Not supported               | ✅ Native handler    |
| Heartbeats (HEARTBEAT_OK)      | ❌ LLM loop only               | ✅ Proper heartbeat  |
| Emoji reactions                | ❌ No channel context          | ✅ Full reaction API |
| Session continuity             | ⚠️ Via `x-openclaw-session-key` header | ✅ Native session   |
| Tool approval escalation       | ❌ Not reachable               | ✅ Full Clash flow   |

## Tracking

- Branch: `host-agent-v3`
- Related adapter: `src/adapters/openclaw.rs` (`OpenClawHttpAdapter`)
- Test documenting the current limitation:
  `src/router.rs::tests::test_openclaw_http_adapter_does_not_intercept_slash_commands`
