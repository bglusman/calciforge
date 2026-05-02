---
layout: default
title: Codex and OpenClaw Integration
---

# Codex, OpenClaw, and CLI-backed subscriptions

Calciforge supports Codex in two practical ways:

1. direct Codex CLI dispatch with `kind = "codex-cli"`.
2. OpenClaw as an upstream agent or model gateway, using OpenClaw's
   Codex-aware model prefixes.
3. an executable-backed `[[exec_models]]` model-gateway entry when an
   OpenAI-compatible client needs to call a local subscription CLI.

Use direct `codex-cli` when Calciforge should call the official Codex
CLI under the same Unix account that owns `~/.codex` credentials. Use
OpenClaw when you want OpenClaw's richer agent runtime, plugins,
skills, session surfaces, or Codex OAuth routing.

## Direct Codex CLI agent

Authenticate Codex first as the user that will run Calciforge:

```bash
codex login
codex exec --model gpt-5.5 "Say READY"
```

Then configure Calciforge:

```toml
[[agents]]
id = "codex"
kind = "codex-cli"
model = "gpt-5.5"
timeout_ms = 600000
aliases = ["gpt", "openai"]

[[routing]]
identity = "owner"
default_agent = "codex"
allowed_agents = ["codex"]
```

By default the adapter runs:

```bash
codex exec --color never --sandbox read-only --ephemeral --skip-git-repo-check -
```

The prompt is sent on stdin and Calciforge captures Codex's
`--output-last-message` file so channel replies do not include JSONL,
tool events, or terminal status noise.

Override `command` when Codex is not on `PATH`, and override `args`
when you want a different Codex execution profile:

```toml
[[agents]]
id = "codex-workspace"
kind = "codex-cli"
command = "/Applications/Codex.app/Contents/Resources/codex"
model = "gpt-5.5"
args = [
  "exec",
  "--color", "never",
  "--sandbox", "workspace-write",
  "--ephemeral",
  "--skip-git-repo-check",
  "-",
]
```

Do not wrap Codex CLI with generic `HTTP_PROXY`/`HTTPS_PROXY` unless you have
validated that specific route. Codex uses streaming and browser/OAuth-backed
control-plane calls; the current `security-proxy` does not inspect CONNECT
tunnels and can break those flows. Use Calciforge's OpenAI-compatible gateway,
exec models, explicit fetch/tool integration, or audited recipes for traffic
that needs scanning or secret substitution.

Keep chat-facing Codex agents conservative. `read-only` is the safer
default for general messaging channels. Use `workspace-write` only for
trusted identities or dedicated coding channels.

## OpenClaw Codex routes

OpenClaw distinguishes three OpenAI-family paths:

| Model ref | Runtime path | Use when |
|---|---|---|
| `openai/gpt-5.4` | Direct OpenAI Platform API | You have an `OPENAI_API_KEY` and want usage-based API billing. |
| `openai-codex/gpt-5.4` | Codex/ChatGPT OAuth provider | You want Codex subscription/OAuth access without embedding the Codex harness. |
| `codex/gpt-5.4` | OpenClaw bundled Codex harness | You want OpenClaw to run an embedded Codex app-server turn. |

For Calciforge, those routes normally sit behind an OpenClaw adapter. Prefer
`openclaw-channel` when the Calciforge bridge plugin is installed in OpenClaw
and can callback to Calciforge. The name is historical: Calciforge owns the
human-facing channel, while OpenClaw owns the selected agent lane and runtime.
Calciforge intentionally does not support OpenClaw agent chat through the
OpenAI-compatible `/v1/chat/completions` endpoint.

```toml
[[agents]]
id = "openclaw-codex"
kind = "openclaw-channel"
endpoint = "http://127.0.0.1:18789"
api_key = "{{secret:OPENCLAW_GATEWAY_TOKEN}}"
reply_auth_token_file = "~/.config/calciforge/secrets/openclaw-reply-token"
openclaw_agent_id = "codex"
timeout_ms = 600000
aliases = ["oc-codex"]
```

For installer-managed OpenClaw hosts, pass the same token pair to
`calciforge install` so the remote plugin is configured to authenticate
Calciforge inbound requests and authenticate its `/hooks/reply` callbacks:

```sh
calciforge install \
  --calciforge-host calciforge@calciforge.lan \
  --claw 'name=openclaw-codex,adapter=openclaw-channel,host=root@openclaw.lan,endpoint=http://openclaw.lan:18789,auth_token=REPLACE_WITH_INBOUND_TOKEN,reply_webhook=http://calciforge.lan:18797/hooks/reply,reply_auth_token=REPLACE_WITH_REPLY_TOKEN'
```

The older `openclaw-native` `/hooks/agent` path is for hook-style automation.
On current OpenClaw it may acknowledge with only a `runId` and complete
asynchronously, so it is not a reliable inline reply adapter by itself.

OpenClaw owns the Codex provider configuration; Calciforge owns the
identity, channel, secret-substitution, and policy boundaries.

### Callback attachments

The `openclaw-channel` reply webhook accepts the original text-only callback:

```json
{ "sessionKey": "calciforge:codex:brian", "message": "done" }
```

It also accepts inline attachment payloads for generated images, diagrams,
reports, or other files:

```json
{
  "sessionKey": "calciforge:codex:brian",
  "message": "I made a diagram.",
  "attachments": [
    {
      "name": "diagram.png",
      "mimeType": "image/png",
      "caption": "Architecture sketch",
      "dataBase64": "iVBORw0KGgo="
    }
  ]
}
```

Calciforge writes inline data into its own artifact storage before channels see
it. Attachment names are sanitized, local paths are not exposed in fallback
text, and malformed attachment payloads fail the pending dispatch instead of
hanging. Callback artifacts share the same local cleanup policy as artifact CLI
recipes: new runs opportunistically prune run directories older than 24 hours.
If the OpenClaw bridge completes a run but has no visible text or attachment to
return, it should callback with an `error` field for the correlated request so
Calciforge can fail immediately instead of waiting for the full adapter timeout.
Remote URL ingestion is intentionally not part of this callback contract yet; it
needs an explicit SSRF-safe policy and should prefer local push/upload or
short-lived signed URLs over arbitrary fetches.

## Claude Code CLI path

For Claude subscriptions, prefer the official Claude Code CLI path when
the operator has confirmed that route is allowed for their account:

```toml
[[agents]]
id = "claude-cli"
kind = "cli"
command = "claude"
args = ["-p", "{message}", "--output-format", "text"]
timeout_ms = 600000
aliases = ["claude", "sonnet"]
```

`acpx` remains useful for ACP compatibility and persistent acpx
sessions, but it is no longer the only local-CLI bridge Calciforge can
use.

## Model gateway expectations

Calciforge's OpenAI-compatible model gateway can forward HTTP requests to
OpenAI-compatible providers, or it can expose trusted local CLI wrappers as
`[[exec_models]]`.

Use an exec model when the subscription-owning CLI should remain a black
box and Calciforge only needs to render a prompt, invoke the process, and
wrap the final text as a chat completion:

```toml
[proxy]
enabled = true
bind = "127.0.0.1:8080"

[[exec_models]]
id = "claude/sonnet"
name = "Claude subscription CLI"
context_window = 200000
command = "/etc/calciforge/exec-models/claude-print.sh"
timeout_seconds = 900

[exec_models.env]
CALCIFORGE_CLAUDE_MODEL = "sonnet"
```

The example wrappers in `scripts/exec-models/` are intentionally conservative
starting points. CLI flags, installed versions, and vendor subscription terms
can change, so validate the exact wrapper under the Unix account running
Calciforge before exposing it to agents. Prefer wrappers that accept prompts on
stdin; if a vendor CLI only accepts prompts as argv, treat that wrapper as a
local process-listing leakage risk and expose it only on trusted single-user
hosts.

For deterministic gateway tests, use a mock OpenAI-compatible provider or
replay fixture rather than a live LLM.
