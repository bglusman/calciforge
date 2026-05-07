---
layout: default
title: Model Gateway
---

# Model Gateway

Calciforge can expose an OpenAI-compatible local endpoint while routing
requests across upstream providers, local models, aliases, and synthetic
model choices.

Agents can also point at an OpenAI-compatible endpoint with
`kind = "openai-compat"`. Use that for plain model-gateway or model API
targets. Do not use it as an OpenClaw agent adapter; OpenClaw agents should use
`kind = "openclaw-channel"` so slash commands and agent identity stay native.
Set `allow_model_override = true` only for OpenAI-compatible agents that should
accept Calciforge `!model` selections and gateway model selectors. Leave it unset
for endpoints with their own restricted model namespace.

From a user-experience perspective, keep model routes separate from agents.
Agents own runtime identity, commands, tools, sessions, approvals, memory, and
artifacts. Model routes are just chat/model endpoints. They can be useful for a
simple chatbot lane or dispatcher testing, but they should be shown as "models"
or "chat routes" rather than as full agents in user-facing lists.

## What Exists Today

| Feature | Status | Notes |
|---|---:|---|
| OpenAI-compatible `/v1/chat/completions` proxy | Working | Local endpoint forwards to configured providers. |
| Provider pattern routing | Working | `[[proxy.providers]]` model globs map model names to upstream APIs. |
| Explicit model routes | Working | `[[proxy.model_routes]]` overrides provider pattern matching. |
| Model shortcuts | Working | `[[model_shortcuts]]` gives users short aliases such as `sonnet`. |
| Local model switching | Working | `[local_models]` manages local `mlx_lm.server` targets. |
| Alloys | Working | `[[alloys]]` samples among interchangeable constituents by `weighted` or `round_robin` strategy, with context-window safety checks. |
| Fallback behavior | Working, implicit | Alloy execution produces an ordered attempt plan; later constituents are tried when earlier ones fail. |
| Named cascades | Working | `[[cascades]]` defines explicit ordered fallback chains and skips targets whose declared context window cannot fit the request. |
| Dispatchers | Working | `[[dispatchers]]` picks the smallest configured context window that fits, then uses larger eligible models as fallbacks. |
| Token estimators | Working | `char_ratio`, `byte_ratio`, and optional `tiktoken-rs` support for OpenAI-compatible BPE counts. |
| CLI-backed subscription agents | Working | Codex, Claude Code, Kimi Code, Dirac, and generic executable adapters are agent routes, not gateway model selectors. |
| External gateway metadata | Working | `/gateway`, `/gateway/ui`, and `!gateway` expose the selected gateway engine and operator dashboard link after sender identity resolution. |
| Helicone external gateway adapter | Working | `backend_type = "helicone"` forwards OpenAI-compatible requests to a Helicone AI Gateway while preserving Calciforge auth, routing, and command UX. |

## External Gateway Engines

Calciforge's gateway layer is pluggable at the engine boundary. The built-in
`mock` and `http` engines remain useful for local development and direct
provider forwarding. External engines can add operator-facing dashboards or
provider management without changing how channels and agents talk to
Calciforge.

Helicone is the first external gateway adapter. Calciforge's installer can
provision a local Helicone deployment when `CALCIFORGE_HELICONE_ENABLED=true`.
The tested local setup uses Helicone's all-in-one Docker image for the
dashboard, local storage, and Jawn API, plus the standalone
`@helicone/ai-gateway` package for request routing. The standalone gateway is
intentional: current all-in-one images may start a bundled gateway supervisor
that exits before routing traffic.
The installer pins the dashboard image with `CALCIFORGE_HELICONE_IMAGE`
(`helicone/helicone-all-in-one:v2025.08.21` by default) so local installs do
not drift when upstream retags `latest`.

Configure Calciforge manually by setting `backend_type = "helicone"` and
pointing `backend_url` at the Helicone AI Gateway OpenAI-compatible base URL.
`backend_url` must be a plain `http` or `https` base URL without query
parameters or fragments.
If it has no path, Calciforge posts to `/v1/chat/completions`; if it already
includes a path such as `/v1`, `/ai`, or `/router/<name>`, Calciforge appends
`/chat/completions` to that configured base path instead of injecting another
`/v1`.

```toml
[proxy]
enabled = true
bind = "127.0.0.1:8080"
api_key_file = "/etc/calciforge/secrets/model-gateway-client-key"
backend_type = "helicone"
backend_url = "http://127.0.0.1:8787/ai"
backend_api_key_file = "/etc/calciforge/secrets/helicone-gateway-key"
gateway_ui_url = "http://127.0.0.1:3300"
```

For a LAN-visible local dashboard during install:

```bash
CALCIFORGE_HELICONE_ENABLED=true \
CALCIFORGE_HELICONE_DASHBOARD_ENABLED=true \
CALCIFORGE_HELICONE_DASHBOARD_BIND=0.0.0.0 \
CALCIFORGE_HELICONE_DASHBOARD_USER_EMAIL=you@example.com \
CALCIFORGE_HELICONE_DASHBOARD_PASSWORD_FILE=/path/to/dashboard-password \
bash scripts/install.sh --yes
```

The default dashboard bind is `127.0.0.1`. Use `0.0.0.0` only on a trusted LAN
or behind WireGuard. Bind addresses decide where local services listen; they
are not necessarily the URLs users should click from another device.

After installing or repairing the local Helicone stack, run the focused doctor
to verify the same path a remote browser uses:

```bash
CALCIFORGE_HELICONE_DASHBOARD_USER_EMAIL=you@example.com \
scripts/helicone-doctor.sh
```

The script checks the dashboard URL, browser-visible Jawn endpoint, published
ports, dashboard credential account, gateway API-key permissions, and whether
ClickHouse rows are visible to the configured dashboard user's organization.

When a dashboard user email is provided, the installer attaches the local
gateway API key to that user's Helicone organization. It creates or repairs the
credential account only when `CALCIFORGE_HELICONE_DASHBOARD_PASSWORD` or
`CALCIFORGE_HELICONE_DASHBOARD_PASSWORD_FILE` is set; otherwise it only attaches
to an existing dashboard user and falls back to the service-owned local org.
The local gateway key is seeded with read/write permissions because Helicone's
AI Gateway needs read access to its control-plane signed-URL endpoint before it
can persist request/response bodies.

Set `gateway_ui_url` to the externally reachable dashboard URL you operate,
such as a Tailscale MagicDNS name, Tailscale IP, WireGuard address, or
authenticated reverse-proxy URL:

```toml
[proxy]
gateway_ui_url = "https://calciforge-gateway.example.invalid"
```

The installer writes the same setting from `CALCIFORGE_GATEWAY_UI_URL` and
does not require Calciforge to own the tunnel, DNS name, certificate, firewall,
or reverse proxy. If `CALCIFORGE_GATEWAY_UI_URL` is unset, the installer only
records a local dashboard URL when it actually starts the local dashboard
container. When a dashboard URL is configured, `!gateway` and `/gateway` expose
it so the operator can jump from Calciforge into Helicone's observability UI.

Use the same pattern for other local web surfaces: keep the service bind
conservative, then configure the advertised public URL separately. Paste-server
links use `CALCIFORGE_PASTE_PUBLIC_BASE_URL` for reverse proxies or tunnels and
`CALCIFORGE_PASTE_PUBLIC_HOST` for a stable LAN/Tailscale host.

The Helicone gateway is currently strongest for providers that Helicone knows
how to route directly, such as Ollama via `/ollama/v1`. Arbitrary
OpenAI-compatible providers may still be configured as direct Calciforge
providers until their Helicone provider/converter support is validated.

`!gateway` is handled only after a channel resolves the sender identity. It can
include internal bind addresses or dashboard URLs, so room-based channels and
future pairing flows should keep their own authorization semantics rather than
reusing trusted-owner DM assumptions.

For process-boundary coverage, run:

```bash
python3 scripts/model-gateway-helicone-smoke.py
```

That script starts a local Helicone-shaped gateway, starts Calciforge in
`--proxy-only` mode, checks `/gateway` metadata and `/gateway/ui`, and sends a
real `/v1/chat/completions` request through Calciforge to prove the adapter
forwards the expected auth headers, path, and model.

## Model Selection

`!model` has two related surfaces:

- `!model` or `!model list` renders activatable choices for channels that can
  show buttons, with numbered text fallbacks everywhere else.
- `!model use <id>` stores the selected model for the sender identity. Shortcut
  aliases such as `!model sonnet` resolve to their configured target before
  storage. Adapters receive the selected target only when their config
  explicitly allows model overrides.

### Model Identifier Resolution

The gateway treats model identifiers uniformly across direct API calls,
`!model` overrides, routing selectors, and provider routing:

- A model identifier may be a shortcut alias, a synthetic routing selector, a
  local model ID, or a concrete upstream model ID.
- `[[model_shortcuts]]` may target concrete provider models, synthetic routing
  selectors such as dispatchers/cascades/alloys, or local model IDs.
- Shortcut aliases are themselves public model IDs. Calciforge rejects aliases
  that collide with configured synthetic routing selectors, local model IDs,
  exact provider model IDs, exact `[[proxy.model_routes]]` patterns, agent IDs,
  or agent aliases.
- Exact model IDs also share the operator-facing selector namespace with agent
  IDs and agent aliases. Calciforge rejects a model route or provider model
  named like an agent selector, because that usually means an agent name has
  been accidentally treated as a concrete gateway model.
- Synthetic routing constituents may also use shortcut aliases. Before provider
  routing, Calciforge expands aliases and nested routing selectors through the
  shared model resolver until the route plan contains terminal gateway model
  IDs. Terminal IDs route to provider gateways or local model endpoints.
- Shortcut cycles and synthetic cycles fail closed instead of falling through to
  a backend as ambiguous model names.
- Proxy model access is checked twice: first for the requested/root model, then
  again for every concrete model in the expanded route plan. `blocked_models`
  therefore applies to concrete downstream models even when a request entered
  through an allowed dispatcher or alias.

Exact model IDs listed in `[[proxy.providers]].models` are activatable choices.
Wildcard patterns such as `openai/*` still route gateway requests, but they are
not shown as tap-to-select model choices because there is no concrete model ID
to activate.

## Synthetic Routing Selectors

Calciforge uses "synthetic routing selector" to mean "a model name that
represents routing logic, not a single upstream model ID." There are three
intended classes: alloys, cascades, and dispatchers. They may reference other
synthetic routing selectors as long as the resulting graph is a DAG; cycles
fail config initialization.

### Alloy

An alloy blends equivalent models. It is useful when any constituent
can answer the request and the operator wants a cost, latency, or
quality mix.

Alloy constituents must be context-compatible. In current code, every
constituent declares `context_window`, and the alloy has an effective
minimum context window. If `min_context_window` is configured, every
constituent must meet or exceed it. If it is omitted, Calciforge
auto-computes the alloy ceiling as the smallest constituent window.
That means mixed-window constituents are allowed only when the alloy
is willing to behave as if it had the smallest window in the group.
For "small request goes local, large request goes remote," use a
dispatcher instead of an alloy.

```toml
[[alloys]]
id = "balanced"
name = "Balanced remote blend"
strategy = "weighted"
min_context_window = 100000

[[alloys.constituents]]
model = "anthropic/claude-sonnet-4.6"
weight = 70
context_window = 200000

[[alloys.constituents]]
model = "openrouter/google/gemini-flash-1.5"
weight = 30
context_window = 100000
```

Current behavior:

- `weighted` samples without replacement for the request.
- `round_robin` rotates the primary constituent.
- every constituent declares `context_window`.
- `min_context_window` is explicit or auto-computed as the minimum
  declared constituent window.
- a constituent below explicit `min_context_window` fails config load.
- the selected model is tried first; remaining constituents become the
  fallback order for that request.

### Cascade

A cascade is an ordered fallback chain: try A, then B, then C on
timeout, 429, 5xx, or other retryable provider failure.

This behavior exists today inside alloy execution, because an alloy
selection returns an ordered list of constituents and the proxy tries
them in order. Named `[[cascades]]` make that behavior explicit
without requiring weighted or round-robin selection. The proxy skips a
cascade target when the request estimate plus output budget exceeds
that target's declared `context_window`.

```toml
[[cascades]]
id = "local-then-remote"
name = "Local first, remote fallback"

[[cascades.models]]
model = "local/qwen3-35b"
context_window = 32768

[[cascades.models]]
model = "anthropic/claude-sonnet-4.6"
context_window = 200000
```

### Dispatcher

A dispatcher chooses a target by request shape. The primary planned
case is "smallest sufficient model": use local/cheap models for small
requests, promote to larger-context or higher-quality models only when
the prompt no longer fits.

The settled name is **dispatcher**, not router, because "router" is
already overloaded by HTTP routing, channel routing, and provider
routing in the codebase.

Dispatchers are implemented as `[[dispatchers]]`. Each target declares
`context_window`; at runtime the gateway estimates the request size,
reserves the requested output budget, and tries the smallest target
that can hold the total. Larger eligible targets become the fallback
order.

```toml
[[dispatchers]]
id = "smart-local"
name = "Use local until the prompt outgrows it"

[[dispatchers.models]]
model = "local/qwen3-35b"
context_window = 32768

[[dispatchers.models]]
model = "openrouter/google/gemini-flash-1.5"
context_window = 100000

[[dispatchers.models]]
model = "anthropic/claude-sonnet-4.6"
context_window = 200000
```

## CLI-Backed Agents

Executable CLIs are Calciforge agents, not model gateway selectors. Use
`kind = "codex-cli"`, `kind = "claude-cli"`, `kind = "kimi-cli"`,
`kind = "dirac-cli"`, `kind = "exec"`, or `kind = "cli"` when a local
subscription-owning CLI should keep its own login, session state, and native
workflow. Those adapters receive chat messages through the normal agent router,
can participate in `!agent` / `!sessions` / `!btw`, and are intentionally kept
out of `/v1/models` so agents and models do not share an ambiguous namespace.

Generic `exec` / `cli` adapters support `{message}`, `{model}`, `{session}`,
and `{session_uuid}` placeholders in args and environment values. First-class
adapters add safer defaults for their CLIs: Codex uses stdin and
`--output-last-message`, Claude Code uses print mode and `--session-id`, and
Kimi Code uses quiet print mode with explicit `--session` plus configurable
thinking flags. Example wrapper scripts live in `scripts/cli-agents/`.

If an OpenAI-compatible client needs access to a subscription-backed model,
prefer a provider route through a real gateway or a dedicated OpenAI-compatible
agent endpoint. Do not model local CLIs as gateway models unless that boundary
has been reintroduced deliberately with a clear observability and security
contract.

## Config Example

```toml
[proxy]
enabled = true
bind = "127.0.0.1:8080"
backend_type = "http"
backend_url = "https://api.openai.com/v1"
backend_api_key_file = "/etc/calciforge/secrets/openai-key"

[proxy.token_estimator]
strategy = "auto"        # auto, char_ratio, byte_ratio, or tiktoken
# tokenizer = "o200k_base" # optional tiktoken base override for non-OpenAI IDs
safety_margin = 1.10

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

[[proxy.model_routes]]
pattern = "coding/default"
provider = "anthropic"

[[model_shortcuts]]
alias = "sonnet"
model = "anthropic/claude-sonnet-4.6"

[[model_shortcuts]]
alias = "local"
model = "local/qwen3-35b"

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

[[dispatchers]]
id = "smart-local"
name = "Use local until the prompt outgrows it"

[[dispatchers.models]]
model = "local" # shortcut aliases are valid inside synthetic definitions
context_window = 32768

[[dispatchers.models]]
model = "anthropic/claude-sonnet-4.6"
context_window = 200000
```

## Notes

- Codex, Claude, Kimi, and other subscription-backed CLI routes are configured
  as agent integrations. See
  [Codex/OpenClaw integration](codex-openclaw-integration.html) for direct
  `codex-cli`, `claude-cli`, `kimi-cli`, OpenClaw `openai-codex/*`, and
  OpenClaw `codex/*` setup choices.
- The model gateway uses a shared `TokenEstimator` trait for fit
  checks. The default `auto` strategy uses `tiktoken-rs` for recognized
  OpenAI-compatible model names when Calciforge is built with
  `--features tiktoken-estimator`, then falls back to the conservative
  char-ratio estimator.
- For non-OpenAI models where an exact provider tokenizer is not
  available, operators can still choose `strategy = "tiktoken"` with
  `tokenizer = "o200k_base"` or `tokenizer = "cl100k_base"` to get a
  real BPE tokenization pass instead of a pure ratio heuristic. Treat
  that as routing-grade, not billing-grade, for Claude, Gemini, Kimi,
  Qwen, or other tokenizer families.
- `char_ratio` and `byte_ratio` remain useful when a deployment wants a
  tiny dependency set or a deliberately conservative approximation for
  code-heavy, mixed-language, or unknown local-model traffic.
- Request-fit checks compare estimated input plus output budget
  against each target's declared context window.
- Provider routes and local model switching are intentionally separate:
  provider routes decide where an OpenAI-style request goes; local
  switching decides which local model process is loaded.
