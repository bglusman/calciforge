# Calciforge

> **Keep your castle secure and moving.**

Calciforge is a self-hosted security gateway for AI agents. It sits
between your agents and the rest of the world, so every agent gets its
own model routes, command permissions, destination-scoped secret
substitution, and audit trail without holding your raw API keys.

The longer feature tour, configuration examples, and architecture notes
live on the docs site: **[calciforge.org](https://calciforge.org/)**.

## What Works Today

| Area | Status | Where to read more |
|---|---:|---|
| `{{secret:NAME}}` substitution in URL, headers, and body | Working | [Secret management](https://calciforge.org/#secret-management) |
| Per-secret destination allowlists | Working | [Outbound traffic gating](https://calciforge.org/#outbound-traffic-gating) |
| Local paste UI for one-shot and bulk `.env` secret input | Working | [Secret management](https://calciforge.org/#secret-management) |
| MCP and CLI tools for agent-facing secret-name discovery, with no value readback | Working | [Agent-facing tools](https://calciforge.org/#agent-facing-tools-mcp) |
| Telegram, Matrix, WhatsApp, and Signal routing | Working | [Multi-channel chat](https://calciforge.org/#multi-channel-chat) |
| OpenAI-compatible model gateway, provider routing, model aliases, alloys, cascades, dispatchers, exec models, and local model switching | Working | [Model gateway](docs/model-gateway.md) |
| Codex CLI and OpenClaw Codex subscription/OAuth integration paths | Working | [Codex integration](docs/codex-openclaw-integration.md) |
| Inbound prompt-injection scanning and outbound exfiltration-pattern scanning | Working | [Traffic gating](https://calciforge.org/#outbound-traffic-gating) |
| [`clash`](https://crates.io/crates/clash)-backed tool policy via the `clashd` sidecar | Working | [Policy sidecar](crates/clashd/README.md) |
| mTLS `host-agent` for ZFS, systemd, PCT, git, and exec operations | Working | [Host-agent](crates/host-agent/README.md) |
| Slack/Discord team ChatOps and Castle-to-Castle federation | Roadmap | [Team ChatOps sketch](docs/roadmap/team-chatops-slack-discord.md) |
| Per-agent secret ACLs beyond destination allowlists | Roadmap | [Secret access policy](docs/roadmap/agent-secret-access-policy.md) |

## Quick Start

```bash
git clone https://github.com/bglusman/calciforge
cd calciforge
brew install fnox
fnox init
bash scripts/install.sh
```

After install, the default local pieces are:

- `calciforge` — channel router, commands, identity, model gateway
- `security-proxy` on `127.0.0.1:8888` — substitution, destination checks, scanning, credential injection
- `clashd` on `127.0.0.1:9001` — small HTTP adapter around the `clash` policy engine
- `secrets-client` — env → fnox → Vaultwarden secret resolver
- `calciforge-secrets` — non-MCP secret-name discovery and `{{secret:NAME}}` reference helper
- `paste-server` — short-lived local forms for adding secrets without putting values in chat history

Channel-based secret input is intentionally being de-emphasized. It
may remain as a per-channel opt-in fallback for travel, low-stakes
keys, or values you plan to rotate soon, but direct `fnox` input and
the local web UI are the preferred paths. The risk varies by channel:
self-hosted encrypted Matrix is the least bad, Signal is still a
chat-history tradeoff, and Telegram is a poor place for raw secrets.

Route Claude Code or another HTTP-speaking agent through the gateway:

```bash
export HTTPS_PROXY=http://localhost:8888
```

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
- [Channel secret-input deprecation note](docs/roadmap/channel-secret-input-deprecation.md)
- [Internal research and planning notes](research/)

## License

MIT. Some bundled tools, including fnox, carry their own licenses; see
the relevant crate manifests and upstream projects.
