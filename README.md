# Calciforge

> **Keep your castle secure and moving.**

Calciforge is a self-hosted security gateway for AI agents. It sits
between your agents and the rest of the world, so every agent gets its
own model routes, command permissions, destination-scoped secret
substitution, and audit trail without holding your raw API keys.

The longer feature tour, configuration examples, and architecture notes
live on the docs site: **[calciforge.org](https://calciforge.org/)**.

## What Works Today

This is usable for a solo operator, but still in active hardening. New
installations should be smoke-tested against their real channel
credentials, fnox store, gateway providers, and synthetic routes before
being treated as daily-driver infrastructure.

| Area | Status | Where to read more |
|---|---:|---|
| `{{secret:NAME}}` substitution in URL, headers, and body | Working | [Secret management](https://calciforge.org/#secret-management) |
| Per-secret destination allowlists | Working | [Outbound traffic gating](https://calciforge.org/#outbound-traffic-gating) |
| Local paste UI for one-shot and bulk `.env` secret input | Working | [Secret management](https://calciforge.org/#secret-management) |
| MCP and CLI tools for agent-facing secret-name discovery, with no value readback | Working | [Agent-facing tools](https://calciforge.org/#agent-facing-tools-mcp) |
| Telegram, Matrix, WhatsApp, Signal, and text/iMessage routing | Working | [Multi-channel chat](https://calciforge.org/#multi-channel-chat) |
| OpenAI-compatible model gateway, provider routing, model aliases, alloys, cascades, dispatchers, exec models, and local model switching | Working | [Model gateway](docs/model-gateway.md) |
| Codex CLI and OpenClaw Codex subscription/OAuth integration paths | Working | [Codex integration](docs/codex-openclaw-integration.md) |
| `calciforge doctor` config/state/endpoint diagnostics | Working | [Quick Start](#quick-start) |
| Inbound prompt-injection scanning and outbound exfiltration-pattern scanning via editable default Starlark policy | Working | [Traffic gating](https://calciforge.org/#outbound-traffic-gating) |
| Configurable scanner checks with editable Starlark policy, Rust-backed `regex_match`, and remote HTTP/LLM extension points | Working | [Security gateway](docs/security-gateway.md) |
| Contributor red-team fixtures for prompt-injection, encoding, Unicode, and tool-policy bypass cases | Working | [Security gateway](docs/security-gateway.md#testing) |
| [`clash`](https://crates.io/crates/clash)-backed tool policy via the `clashd` sidecar | Working | [Policy sidecar](crates/clashd/README.md) |
| mTLS `host-agent` for ZFS, systemd, PCT, git, and exec operations | Working | [Host-agent](crates/host-agent/README.md) |
| Slack/Discord team ChatOps and Castle-to-Castle federation | Roadmap | [Team ChatOps sketch](docs/roadmap/team-chatops-slack-discord.md) |
| Per-agent secret ACLs beyond destination allowlists | Roadmap | [Secret access policy](docs/roadmap/agent-secret-access-policy.md) |

## Quick Start

```bash
git clone https://github.com/bglusman/calciforge
cd calciforge
bash scripts/install.sh
calciforge doctor
```

After install, the default local pieces are:

- `calciforge` — channel router, commands, identity, model gateway
- `security-proxy` on `127.0.0.1:8888` — substitution, destination checks, scanning, credential injection
- `clashd` on `127.0.0.1:9001` — small HTTP adapter around the `clash` policy engine
- `secrets-client` — env → fnox → Vaultwarden secret resolver
- `calciforge-secrets` — non-MCP secret-name discovery and `{{secret:NAME}}` reference helper
- `paste-server` — short-lived local/LAN forms for adding secrets without putting values in chat history

The installer attempts to install and initialize `fnox` automatically.
Calciforge and the `fnox` CLI can share the same `fnox.toml` and
profile, so using `fnox set/list/tui` manually is a valid way to manage
the same store Calciforge resolves through. The paste UI currently
stores through that configured local backend.

The installer runs `calciforge doctor --no-network` after installing
local services when a config file is present. Run `calciforge doctor`
again after editing config or moving services. It
validates config, checks referenced secret files without printing
values, flags stale active-agent/model state, detects suspicious
self-routing into the local model gateway, warns if the Calciforge
service itself has ambient proxy env, flags subprocess agents that explicitly
set proxy env, warns about externally managed agent daemons
whose outbound proxy environment cannot be proven,
validates configured scanner policy files and rule syntax, and can probe
configured agent endpoints. Use `--no-network` for a purely local check.

Channel-based secret input is intentionally being de-emphasized because
chat transports can retain plaintext values. Prefer the paste UI
(`!secure input NAME` / `!secure bulk LABEL` from chat, or
`paste-server NAME` on the host) or direct `fnox` input for new secrets.
Chat-started paste links are intended for browsers on the same local
network unless you configure an authenticated reverse proxy/tunnel with
`CALCIFORGE_PASTE_PUBLIC_BASE_URL`.

Do not put proxy variables on the Calciforge daemon itself; that can route
Calciforge's own provider and control-plane traffic through its security proxy.
Do not assume CLI agents can be wrapped by setting `HTTP_PROXY` or
`HTTPS_PROXY`; Codex, Claude, ACPX, npm-backed adapters, and streaming clients
may use CONNECT, WebSockets, or browser-backed auth flows that the current
proxy cannot inspect and may break. Use OpenAI-compatible gateway routes,
explicit fetch/tool integrations, audited recipes, or tested wrappers for
traffic that must pass through `security-proxy`.

For externally managed agent daemons that Calciforge does not launch, proxying
has to be configured on that daemon or its service manager and validated
against `security-proxy` logs:

```bash
export HTTP_PROXY=http://127.0.0.1:8888
export NO_PROXY=localhost,127.0.0.1,::1
```

Do not treat ambient `HTTPS_PROXY` as a security boundary. HTTPS clients use
CONNECT tunnels; the current proxy does not terminate those tunnels or scan the
encrypted payload. Use a Calciforge-owned model gateway, fetch/tool path, or
audited recipe when HTTPS content needs scanning or secret substitution.

## Tiny Config Sketch

```toml
[calciforge]
version = 2

[[identities]]
id = "owner"
aliases = [{ channel = "telegram", id = "7000000001" }]
role = "owner"

[[agents]]
id = "codex"
kind = "codex-cli"
model = "gpt-5.5"
timeout_ms = 600000

[[routing]]
identity = "owner"
default_agent = "codex"
allowed_agents = ["codex"]

[proxy]
enabled = true
bind = "127.0.0.1:8080"
backend_type = "http"
backend_url = "https://api.openai.com/v1"
backend_api_key_file = "/etc/calciforge/secrets/openai-key"

[proxy.token_estimator]
strategy = "auto"

[[model_shortcuts]]
alias = "sonnet"
model = "anthropic/claude-sonnet-4.6"

[[exec_models]]
id = "codex/gpt-5.5"
name = "Codex GPT-5.5 subscription"
context_window = 262144
command = "/etc/calciforge/exec-models/codex-exec.sh"
args = ["-"]
```

## Architecture

```text
chat channels ─▶ calciforge ─▶ agent
                    │            │
                    │            ▼
                    │      security-proxy ─▶ upstream APIs / web
                    │            │
                    │            ├─ secrets-client / fnox
                    │            ├─ adversary-detector
                    │            └─ clashd policy sidecar
                    │
                    └─ host-agent for narrow system operations
```

The key rule: agents ask for capabilities by name; Calciforge decides
whether the current identity, destination, and policy context allow the
operation.

## Development

```bash
cargo test
cargo test -p calciforge
cargo test -p calciforge --features tiktoken-estimator
cargo test -p secrets-client
cargo fmt --all -- --check
cargo clippy --all-targets
```

Install hooks once:

```bash
bash scripts/install-git-hooks.sh
```

## Docs

- [Feature tour and install notes](https://calciforge.org/)
- [Model gateway reference](docs/model-gateway.md)
- [Codex/OpenClaw integration](docs/codex-openclaw-integration.md)
- [Model gateway RFC](docs/rfcs/model-gateway-primitives.md)
- [Security proxy docs](docs/security-gateway.md)
- [Host-agent docs](crates/host-agent/README.md)
- [Roadmap](docs/roadmap/)
- [Staging test matrix](docs/staging-test-matrix.md)
- [Channel secret-input deprecation note](docs/roadmap/channel-secret-input-deprecation.md)

## License

MIT. Some bundled tools, including fnox, carry their own licenses; see
the relevant crate manifests and upstream projects.
