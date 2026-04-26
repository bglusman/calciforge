# Codex, OpenClaw, and CLI-backed subscriptions

Calciforge supports Codex in two practical ways:

1. direct Codex CLI dispatch with `kind = "codex-cli"`.
2. OpenClaw as an upstream agent or model gateway, using OpenClaw's
   Codex-aware model prefixes.

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

For Calciforge, those routes normally sit behind an OpenClaw adapter:

```toml
[[agents]]
id = "openclaw-codex"
kind = "openclaw-native"
endpoint = "http://127.0.0.1:18789"
api_key = "{{secret:OPENCLAW_HOOK_TOKEN}}"
openclaw_agent_id = "codex"
timeout_ms = 600000
aliases = ["oc-codex"]
```

OpenClaw owns the Codex provider configuration; Calciforge owns the
identity, channel, secret-substitution, and policy boundaries.

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

Calciforge's OpenAI-compatible model gateway forwards HTTP requests to
OpenAI-compatible providers. A Codex or Claude subscription is not
automatically an OpenAI-compatible upstream exposed by Calciforge.

For subscription-backed model access, put the subscription-owning CLI or
OpenClaw provider behind a Calciforge agent. For deterministic gateway
tests, use a mock OpenAI-compatible provider or replay fixture rather
than a live LLM.
