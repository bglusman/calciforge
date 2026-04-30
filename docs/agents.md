---
layout: default
title: Agents, Identities, and Routing
---

# Agents, Identities, and Routing

This page covers the three configuration sections that together control who
can talk to Calciforge and which AI backend handles their messages:

- `[[agents]]` — AI backends Calciforge dispatches to
- `[[identities]]` — users and their per-channel aliases
- `[[routing]]` — maps identities to agents

## Architecture

```
Channel message arrives
        │
        ▼
  Identity lookup          [[identities]] — alias (channel + id) → identity
        │
        ▼
  Routing rule             [[routing]]   — identity → default_agent + allowed_agents
        │
        ▼
  Agent dispatch           [[agents]]    — build adapter, send message, return reply
        │
        ▼
  Reply sent back to user
```

---

## Agents (`[[agents]]`)

Each `[[agents]]` entry defines one AI backend. The `kind` field selects the
adapter. All other fields are adapter-specific.

### Common fields

| Field | Required | Default | Description |
|---|---|---|---|
| `id` | yes | — | Unique name used in routing and `!switch` commands |
| `kind` | yes | — | Adapter type (see below) |
| `timeout_ms` | no | adapter default | Per-request timeout in milliseconds |
| `model` | no | — | Model name forwarded to the backend |
| `api_key` | no | — | Bearer token for the backend; overrides `CALCIFORGE_AGENT_TOKEN` |
| `api_key_file` | no | — | Path to file containing the API key (preferred over inline `api_key`) |
| `auth_token` | no | — | Legacy alias for `api_key` (openclaw-channel) |
| `aliases` | no | `[]` | Additional names matched by `!switch` |
| `allow_model_override` | no | adapter default | Whether `!model` overrides from identities are forwarded |
| `registry` | no | — | Optional metadata shown in `!agents` output (see below) |

### `kind = "openclaw-channel"`

HTTP adapter that talks to an OpenClaw or Calciforge gateway running the
channel plugin. The gateway maintains the agent session; Calciforge acts as
the routing and security layer in front of it.

Required: `endpoint`. Recommended: `api_key` or `api_key_file`.

```toml
[[agents]]
id = "librarian"
kind = "openclaw-channel"
endpoint = "http://127.0.0.1:18789"
api_key_file = "~/.calciforge/secrets/librarian-token"
timeout_ms = 120000
aliases = ["lib", "main"]
registry = { display_name = "Librarian", specialties = ["general", "homelab-ops"] }
```

`openclaw_agent_id` (optional) sets the lane id sent to the gateway; defaults
to this agent's `id`.

`reply_port` (optional, default 18797) is the local port Calciforge listens on
for async `/hooks/reply` callbacks when the gateway pushes replies
asynchronously instead of returning them synchronously.

`reply_auth_token` (optional) — bearer token required on incoming
`/hooks/reply` callbacks.

### `kind = "openai-compat"`

Generic OpenAI-compatible HTTP endpoint (Ollama, LM Studio, Anthropic,
Together, any endpoint that accepts `/v1/chat/completions`).

Required: `endpoint`. Recommended: `model`.

```toml
[[agents]]
id = "local-llm"
kind = "openai-compat"
endpoint = "http://127.0.0.1:11434"
model = "llama3.2"
timeout_ms = 180000
allow_model_override = true
```

Without `model`, Calciforge will not forward a model name to the backend
unless `allow_model_override = true` and the identity sets `!model`.

### `kind = "zeroclaw"`

Direct ZeroClaw agent endpoint (legacy; use `openclaw-channel` for new
deployments).

Required: `endpoint`, `api_key`.

```toml
[[agents]]
id = "zeroclaw"
kind = "zeroclaw"
endpoint = "http://127.0.0.1:18792"
api_key_file = "~/.calciforge/secrets/zeroclaw-token"
timeout_ms = 90000
```

### `kind = "cli"`

Spawns a subprocess for each message. The command receives the message via
the argument template: `{message}` in `args` is replaced at dispatch time.

Required: `command`.

```toml
[[agents]]
id = "ironclaw"
kind = "cli"
command = "/usr/local/bin/ironclaw"
args = ["run", "-m", "{message}"]
timeout_ms = 60000
env = { "LLM_BACKEND" = "openai_compatible", "LLM_MODEL" = "kimi-k2.5" }
```

`env` (optional) — extra environment variables passed to the subprocess.

**Security note:** `{message}` in `args` places user content in the process
argv, which is visible in `ps` output and `/proc/<pid>/cmdline` on multi-user
systems. If the message may contain secret values, use a CLI that reads from
stdin instead and pass the message via stdin rather than argv.

### `kind = "acp"`

Persistent-session adapter for ACP-compliant agents (e.g. `claude --acp`,
`opencode acp`). Unlike `cli`, the process stays alive between messages so
session context is preserved.

Required: `command` (the binary to invoke).

```toml
[[agents]]
id = "claude-code"
kind = "acp"
command = "claude"
args = ["--acp"]
model = "claude-sonnet-4-5"
timeout_ms = 300000
aliases = ["cc", "claude"]
registry = { display_name = "Claude Code", specialties = ["coding", "refactoring"] }
```

### `kind = "acpx"`

Like `acp`, but delegates ACP protocol handling to the `acpx` binary, which
supports additional protocol versions. The `command` field holds the agent
name (not a path); `acpx` resolves it.

Required: `command` (agent name passed to acpx).

```toml
[[agents]]
id = "opencode"
kind = "acpx"
command = "opencode"
timeout_ms = 300000
```

### `kind = "codex-cli"` and `kind = "dirac-cli"`

Subprocess adapters for OpenAI Codex CLI and Dirac CLI respectively.
`command` is optional and defaults to the standard binary name. Both support
`model`, `args`, `env`, and `timeout_ms`.

```toml
[[agents]]
id = "codex"
kind = "codex-cli"
model = "codex-mini-latest"
timeout_ms = 120000
```

### Registry metadata

The optional `registry` table is not used at dispatch time — it populates the
`!agents` command output so users can discover available agents.

```toml
[[agents]]
id = "librarian"
kind = "openclaw-channel"
endpoint = "http://127.0.0.1:18789"
api_key_file = "~/.calciforge/secrets/librarian-token"
timeout_ms = 120000

[agents.registry]
display_name = "Librarian"
description = "General-purpose assistant for homelab and daily tasks"
specialties = ["general", "homelab-ops", "research"]
access = ["admin", "user"]
primary_channels = ["telegram", "matrix"]
```

---

## Identities (`[[identities]]`)

An identity is a named user. The `aliases` list maps channel-specific IDs
(phone numbers, Telegram user IDs, Matrix handles) to the identity name.
Routing rules reference the identity `id`.

| Field | Required | Default | Description |
|---|---|---|---|
| `id` | yes | — | Unique identity name used in routing rules |
| `display_name` | no | — | Human-readable name for logs and `!who` output |
| `role` | no | — | Arbitrary role string (e.g. `"admin"`, `"user"`) |
| `aliases` | no | `[]` | Per-channel IDs: `{ channel = "...", id = "..." }` |

Alias `id` format by channel:

| Channel | Alias `id` format | Example |
|---|---|---|
| `telegram` | numeric user ID | `"7000000001"` |
| `matrix` | Matrix user ID | `"@alice:matrix.org"` |
| `whatsapp` | E.164 phone number | `"+15555550001"` |
| `signal` | E.164 phone number | `"+15555550001"` |
| `sms` | E.164 phone number | `"+15555550001"` |

```toml
[[identities]]
id = "operator"
display_name = "Alice"
role = "admin"
aliases = [
    { channel = "telegram", id = "7000000001" },
    { channel = "matrix",   id = "@alice:matrix.org" },
    { channel = "whatsapp", id = "+15555550001" },
    { channel = "signal",   id = "+15555550001" },
]
```

---

## Routing (`[[routing]]`)

Each routing rule maps one identity to a default agent and an optional
allowlist of agents they may switch to.

| Field | Required | Default | Description |
|---|---|---|---|
| `identity` | yes | — | Must match an `id` in `[[identities]]` |
| `default_agent` | yes | — | Agent dispatched when no `!switch` is active |
| `allowed_agents` | no | `[]` | Agents the identity may `!switch` to; empty = no restriction (any configured agent, regardless of role) |

```toml
[[routing]]
identity = "operator"
default_agent = "librarian"
allowed_agents = ["librarian", "claude-code", "local-llm"]

[[routing]]
identity = "readonly-user"
default_agent = "librarian"
allowed_agents = ["librarian"]
```

When `allowed_agents` is empty, the identity can switch to any configured
agent — there is no role-based check. Set it explicitly for every identity
that should not have unrestricted agent access.

---

## Full example

Minimal working config combining agents, identities, and routing:

```toml
[calciforge]
version = 2

[[identities]]
id = "operator"
display_name = "Alice"
role = "admin"
aliases = [{ channel = "telegram", id = "7000000001" }]

[[agents]]
id = "librarian"
kind = "openclaw-channel"
endpoint = "http://127.0.0.1:18789"
api_key_file = "~/.calciforge/secrets/librarian-token"
timeout_ms = 120000

[[routing]]
identity = "operator"
default_agent = "librarian"
allowed_agents = ["librarian"]

[[channels]]
kind = "telegram"
enabled = true
bot_token_file = "~/.calciforge/secrets/telegram-token"
```

## Verify

```bash
calciforge doctor   # checks agent reachability and identity/routing consistency
calciforge          # start; send a message from a configured alias
```

`calciforge doctor` warns on common misconfigurations: missing `api_key` on
`openclaw-channel` agents, `openai-compat` without `model`, identities with
no routing rule, and routing rules that reference undefined agents.
