# OpenClaw Channel/Plugin API — Integration Points for NonZeroClawed

_Research date: 2026-03-30_
_Sources: /usr/lib/node_modules/openclaw/docs — plugins/, gateway/, automation/_

---

## Summary Answer

There is **no native "nonzeroclawed channel" type** in OpenClaw's current schema. The right integration point depends on what NonZeroClawed needs to do:

| Goal | Mechanism | Stability |
|------|-----------|-----------|
| Send a message to an agent | `POST /hooks/agent` or `/v1/chat/completions` | Stable |
| Receive agent responses | `/hooks/agent` with `deliver: true` + `to:` target | Stable |
| Add a new inbound channel | Plugin with `channels: ["nonzeroclawed"]` manifest | New/plugin-required |
| Inject system events (wake) | `POST /hooks/wake` | Stable |
| Read/write config | `openclaw config get/set` CLI or file edit | Risky (see Workstream 2) |

---

## Option 1: Webhook Ingress (Recommended Short-Term)

OpenClaw exposes `POST /hooks/agent` — an HTTP endpoint for triggering agent runs.

**Config required** (in `openclaw.json`):
```json5
{
  hooks: {
    enabled: true,
    token: "shared-secret-between-nonzeroclawed-and-openclaw",
    path: "/hooks",
    allowedAgentIds: ["main"],
  },
}
```

**NonZeroClawed → OpenClaw (inbound message)**:
```bash
POST http://<claw>:18789/hooks/agent
Authorization: Bearer <hooks.token>
{
  "message": "User says: hello",
  "agentId": "main",
  "sessionKey": "nonzeroclawed:telegram:+15551234567",   # stable per-user session key
  "wakeMode": "now",
  "deliver": false,          # NonZeroClawed will handle delivery
  "model": "anthropic/claude-sonnet-4-6",
  "timeoutSeconds": 120
}
```

**What comes back**: The agent's response text in the HTTP response body.

**What `deliver: true` does**: OpenClaw can deliver the response itself via whatever channel is configured (last used channel, or a specific `to:` target). This could work as a fallback delivery mechanism if NonZeroClawed wants OpenClaw to own the reply.

**Limitation**: There's no persistent two-way channel. NonZeroClawed sends a request, gets a response — it's fundamentally request/response, not a streaming bidirectional channel.

---

## Option 2: OpenAI Chat Completions (Confirmed Working)

```json5
// Enable in openclaw.json
{
  gateway: {
    http: {
      endpoints: {
        chatCompletions: { enabled: true },
      },
    },
  },
}
```

```bash
POST http://<claw>:18789/v1/chat/completions
Authorization: Bearer <gateway.auth.token>
{
  "model": "openclaw:main",
  "messages": [{"role": "user", "content": "..."}],
  "user": "nonzeroclawed:telegram:+15551234567",  # stable session key derivation
  "stream": false
}
```

**Confirmed working** on live Librarian instance (tested 2026-03-30).

**Advantage over hooks**: The chat completions endpoint is a standard interface — no custom `hooks` config needed on the OpenClaw side. Just need `chatCompletions.enabled: true` and the gateway token.

**Streaming**: Set `"stream": true` → SSE stream. NonZeroClawed can relay tokens to the user as they arrive.

**Session routing**: The `user` field creates a stable session key. If NonZeroClawed consistently sends the same `user` value for the same sender, OpenClaw maintains a persistent session context for that person.

---

## Option 3: Native Plugin Channel (Future / Best-Long-Term)

OpenClaw's plugin system allows registering new channel IDs:

**Plugin manifest** (`openclaw.plugin.json`):
```json
{
  "id": "nonzeroclawed-channel",
  "name": "NonZeroClawed Channel",
  "version": "0.1.0",
  "channels": ["nonzeroclawed"],
  "configSchema": {
    "type": "object",
    "additionalProperties": false,
    "properties": {
      "endpoint": { "type": "string" },
      "token": { "type": "string" }
    }
  }
}
```

**Plugin handler** registers:
- A channel implementation that OpenClaw routes inbound messages through
- The channel sends/receives to NonZeroClawed's endpoint
- OpenClaw treats it like any other channel (Telegram, Signal, etc.)

**What this enables**:
- NonZeroClawed gets proper `channels.nonzeroclawed` config block in `openclaw.json`
- OpenClaw's routing/allowlist/DM policy applies to NonZeroClawed messages
- Session scoping, group policies, mention gating all work correctly
- NonZeroClawed messages appear in OpenClaw's session history properly tagged

**What's required**:
- Write the plugin (TypeScript, registers a channel)
- Install it on the target OpenClaw instance
- Add `channels.nonzeroclawed` config block

**Risk**: This requires modifying `openclaw.json` to add a plugin entry — exactly the operation that broke Librarian on 2026-03-30. Must go through the safe adapter installer flow.

---

## Option 4: Tools Invoke (Read/Observe Only)

`POST /tools/invoke` lets NonZeroClawed call any allowed tool directly:

```bash
POST /tools/invoke
Authorization: Bearer <gateway.auth.token>
{
  "tool": "sessions_list",
  "args": {},
  "sessionKey": "agent:main:main"
}
```

**Available tools** (depends on policy, but `session_status`, `sessions_list`, `memory_search` likely available):
- `session_status` — current session state ✅ confirmed
- `sessions_list` — enumerate sessions
- `sessions_history` — read session transcript
- `memory_search` — search workspace memory

**What this is useful for**:
- NonZeroClawed reading OpenClaw's memory to provide context
- Health monitoring / status checks
- Observing session state during migration

**Default HTTP deny list** (can't call without config override):
- `sessions_spawn`, `sessions_send`, `gateway`, `whatsapp_login`

---

## Recommended Integration Strategy for NonZeroClawed

### Phase 1: No Config Changes Required

Use the chat completions endpoint. Only requirement: `chatCompletions.enabled: true` in config (one field, minimal risk). NonZeroClawed acts as a thin proxy.

```
NonZeroClawed receives message → POST /v1/chat/completions → relay response back
```

### Phase 2: Hooks for Push Events

Enable `hooks.enabled: true` — requires adding a `hooks` block to config. This is a new top-level key, not modifying existing channel config. Lower risk than modifying channel entries.

Enables:
- `/hooks/wake` for system events (e.g. "new email arrived")
- `/hooks/agent` for richer agent runs with session routing
- Delivery back to original channel via `deliver: true`

### Phase 3: Native Plugin Channel

Install the NonZeroClawed plugin on the OpenClaw instance. This requires the full safe installer flow (backup → version check → schema validate → diff → confirm → health check → rollback path).

This is the cleanest long-term architecture but needs the most setup work and is the highest risk during install.

---

## What NonZeroClawed Should NOT Do

Based on the 2026-03-30 incident:

1. **Don't add config keys that don't exist in the schema** — this crashes the gateway
2. **Don't add a `channels.nonzeroclawed` entry without installing the plugin first** — plugin must be installed before its channel ID is referenced
3. **Don't modify `channels.*` entries for existing channels** — that's where live state lives (Telegram bot token, etc.)
4. **Don't modify config without backup + version check** — even correct changes can fail

---

## Hook/Plugin Event Types Available

From `/usr/lib/node_modules/openclaw/docs/automation/hooks.md`:

**Bundled hooks** (events NonZeroClawed could subscribe to):
- `session:new` — when `/new` is issued (session reset)
- `session:reset` — session reset events
- `agent:bootstrap` — agent starts up
- `boot` — gateway boot

**Custom hooks can respond to**:
- `before_agent_start` — inject prompt context (but `allowPromptInjection: false` is default!)
- `after_agent_end` — post-processing after agent turn

**Important**: `plugins.entries.<id>.hooks.allowPromptInjection` defaults to `false`. Core blocks `before_prompt_build` and prompt-mutating fields from `before_agent_start`. NonZeroClawed plugin would not be able to inject into the system prompt without explicit config opt-in.

---

## Sources

- `/usr/lib/node_modules/openclaw/docs/plugins/manifest.md`
- `/usr/lib/node_modules/openclaw/docs/plugins/agent-tools.md`
- `/usr/lib/node_modules/openclaw/docs/gateway/openai-http-api.md`
- `/usr/lib/node_modules/openclaw/docs/gateway/tools-invoke-http-api.md`
- `/usr/lib/node_modules/openclaw/docs/automation/webhook.md`
- `/usr/lib/node_modules/openclaw/docs/automation/hooks.md`
- Live gateway test: `POST /v1/chat/completions` confirmed working on Librarian instance
