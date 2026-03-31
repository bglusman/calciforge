# OpenClaw Migration Research

_Research date: 2026-03-30_
_Note: Sections 7-8 from original draft removed — "transparent runtime fallback" framing was incorrect. See vault-integration-plan.md Workstream 3 for correct framing: PolyClaw routes channels, reassignment is explicit with context handoff._

---

## 1. OpenClaw Data Directory Structure

`~/.openclaw/` on a live instance (Librarian, 2026.3.13):

```
~/.openclaw/
├── openclaw.json                        # Main config (JSON5)
├── agents/
│   ├── main/
│   │   ├── agent/
│   │   │   ├── auth-profiles.json       # LLM provider credentials
│   │   │   └── models.json              # Per-agent model catalog overrides
│   │   └── sessions/
│   │       ├── sessions.json            # Session index (JSON object, 508 sessions)
│   │       └── <uuid>.jsonl             # Individual session transcript (JSONL)
│   ├── david/
│   └── renee/
├── credentials/                         # Channel auth (pairing, allowlists)
│   ├── telegram-allowFrom.json
│   ├── signal-allowFrom.json
│   └── whatsapp/<accountId>/            # Baileys WA session (NON-PORTABLE)
├── cron/
│   └── jobs.json                        # Cron job definitions
└── workspace/                           # Agent workspace root
    ├── AGENTS.md
    ├── MEMORY.md                        # Long-term curated memory (Markdown)
    ├── memory/                          # Daily notes YYYY-MM-DD.md (Markdown)
    └── skills/
```

**Key finding: No SQLite. Everything is flat files.**

---

## 2. Memory Storage (CORRECTED)

- **OpenClaw**: plain text Markdown files in workspace (`MEMORY.md`, `memory/YYYY-MM-DD.md`, etc.)
- **NZC**: SQLite

Migration direction: OpenClaw Markdown → NZC SQLite. Trivially readable — no schema reverse-engineering.

### Recommended Migration Approach

**Phase 1 (Immediate)**: Copy workspace Markdown files verbatim. NZC reads them as context just as OpenClaw does. Zero conversion work.

**Phase 2 (Optional)**: Parse Markdown, insert into NZC SQLite with metadata (date, source file).

**Phase 3 (Bridge)**: During transition, NZC can call `/tools/invoke?tool=memory_search` on the running OpenClaw instance to query old memories without full import.

---

## 3. Session Storage Format

### `sessions.json` — Session Index
Flat JSON object. Keys: `agent:<agentId>:<bucketKey>`. 508 entries on live instance.

### `<uuid>.jsonl` — Session Transcript
Append-only JSONL event log. Event types: `session`, `message`, `model_change`, `thinking_level_change`, `custom:model-snapshot`.

Message event structure:
```json
{
  "type": "message",
  "id": "<event-id>",
  "parentId": "<prev-event-id>",
  "timestamp": "2026-03-02T18:03:48.635Z",
  "role": "user" | "assistant" | "system",
  "content": "<string or content blocks>"
}
```

`id`/`parentId` form a DAG (for compaction branching). Conversion to NZC format needs to linearize this.

---

## 4. Channel Assignment (not migration)

Each channel has exactly one owner. Installation is where you decide.

**What can be migrated**: channel credentials (bot tokens, account IDs, allowlists) for channels PolyClaw/NZC will own.

**What cannot be migrated**:
- WhatsApp (Baileys) session auth — not portable, user must re-link
- Plugin-specific state

Field map for channel credentials:

| OpenClaw field | PolyClaw/NZC equivalent |
|----------------|------------------------|
| `channels.telegram.botToken` | `channels.telegram.token` |
| `channels.telegram.allowFrom` | `channels.telegram.allow_from` |
| `channels.signal.account` | `channels.signal.account` |
| `channels.signal.allowFrom` | `channels.signal.allow_from` |
| `channels.whatsapp.*` | ⚠️ Re-link required |

---

## 5. Config Migration Field Map

### Agent / Model
| OpenClaw | NZC equivalent |
|----------|---------------|
| `agents.defaults.model.primary` | `agent.model` |
| `agents.defaults.model.fallbacks` | `agent.model_fallbacks` |
| `agents.list[].id` | `agents[].id` |
| `agents.defaults.workspace` | `agent.workspace` |
| `agents.defaults.heartbeat.every` | `heartbeat.interval` |

### Gateway
| OpenClaw | NZC equivalent |
|----------|---------------|
| `gateway.port` | `gateway.port` |
| `gateway.bind` | `gateway.bind` |
| `gateway.auth.token` | `gateway.auth.token` |

### API Keys
| OpenClaw | NZC equivalent |
|----------|---------------|
| `env.ANTHROPIC_API_KEY` | `providers.anthropic.api_key` |
| `models.providers.*.apiKey` | `providers.*.api_key` |

### Fields with no NZC equivalent (flag for manual review)
- `plugins.entries.*` — OpenClaw plugins
- `skills.*` — OpenClaw skills system
- `agents.defaults.compaction.*` — OpenClaw-specific
- `hooks.mappings` — OpenClaw webhook config

---

## 6. Context Handoff on Channel Reassignment

When PolyClaw reassigns a channel from OpenClaw to NZC (or vice versa), the new owner needs context. Options:

1. **Recent transcript dump**: PolyClaw reads last N turns from the old claw's session and passes them to the new claw on first message
2. **Memory snapshot**: export the workspace MEMORY.md + recent daily file and inject as system context
3. **Cold start**: new claw starts fresh, user re-establishes context naturally

Option 2 (memory snapshot) is lowest risk and most portable — doesn't require knowing the old claw's session format.

---

## 7. OpenClaw HTTP API (useful for context handoff)

Confirmed working endpoints on live gateway:

- `GET /health` — liveness (no auth)
- `POST /v1/chat/completions` — OpenAI-compatible (auth required, must be enabled in config)
- `POST /tools/invoke` — invoke any allowed tool including `memory_search`, `sessions_history`

These could be used by PolyClaw to fetch context from an OpenClaw instance during reassignment.

---

## 8. Migration Feasibility Summary

| Task | Feasibility | Notes |
|------|-------------|-------|
| Workspace memory (Markdown) | ✅ Trivial | Just files, copy as-is |
| Config migration (channels/models) | ✅ Moderate | Build field-map tool |
| Session history migration | ⚠️ Moderate | JSONL DAG → NZC format |
| WhatsApp session | ❌ Not feasible | Re-link required |
| Config migration (plugins/skills) | ❌ Not feasible | No NZC equivalents |
| Context handoff on reassignment | ✅ Moderate | Memory snapshot approach works |

---

## Sources
- Live `~/.openclaw/` directory inspection (Librarian, 2026.3.13)
- Live gateway probe at `http://127.0.0.1:18789`
- `/usr/lib/node_modules/openclaw/docs/gateway/openai-http-api.md`
- `/usr/lib/node_modules/openclaw/docs/gateway/tools-invoke-http-api.md`
