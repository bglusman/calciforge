# Calciforge

> **Keep your castle secure and moving.**

Calciforge is a self-hosted security gateway for AI agents. It sits
between your agents and the rest of the world, so every agent gets its
own secrets, allowed destinations, model routes, command permissions,
and audit trail without holding your raw API keys.

The longer feature tour, configuration examples, and architecture notes
live on the docs site: **[bglusman.github.io/calciforge](https://bglusman.github.io/calciforge/)**.

## What Works Today

| Area | Status | Where to read more |
|---|---:|---|
| `{{secret:NAME}}` substitution in URL, headers, and body | Working | [Secret management](https://bglusman.github.io/calciforge/#secret-management) |
| Per-secret destination allowlists | Working | [Outbound traffic gating](https://bglusman.github.io/calciforge/#outbound-traffic-gating) |
| `!secure` chat flow plus localhost paste UI for one-shot and bulk `.env` input | Working | [Secret management](https://bglusman.github.io/calciforge/#secret-management) |
| MCP server for agent-facing secret-name discovery, with no value readback | Working | [Agent-facing tools](https://bglusman.github.io/calciforge/#agent-facing-tools-mcp) |
| Telegram, Matrix, WhatsApp, and Signal routing | Working | [Multi-channel chat](https://bglusman.github.io/calciforge/#multi-channel-chat) |
| OpenAI-compatible model gateway, provider routing, model aliases, alloys, and local model switching | Working | [Model gateway](https://bglusman.github.io/calciforge/#model-gateway) |
| Inbound prompt-injection scanning and outbound exfiltration-pattern scanning | Working | [Traffic gating](https://bglusman.github.io/calciforge/#outbound-traffic-gating) |
| [`clash`](https://crates.io/crates/clash)-backed tool policy via the `clashd` sidecar | Working | [Policy sidecar](crates/clashd/README.md) |
| mTLS `host-agent` for ZFS, systemd, PCT, git, and exec operations | Working | [Host-agent](crates/host-agent/README.md) |
| Slack/Discord team ChatOps and Castle-to-Castle federation | Roadmap | [Team ChatOps sketch](docs/roadmap/team-chatops-slack-discord.md) |
| Named cascades and dispatcher routing for the model gateway | RFC | [Model gateway primitives](docs/rfcs/model-gateway-primitives.md) |

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
- `paste-server` — short-lived local forms for adding secrets without putting values in chat history

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
id = "claude"
kind = "cli"
command = "/usr/local/bin/claude"
timeout_ms = 120000

[[routing]]
identity = "owner"
default_agent = "claude"
allowed_agents = ["claude"]

[proxy]
enabled = true
bind = "127.0.0.1:8080"
backend_type = "http"
backend_url = "https://api.openai.com/v1"
backend_api_key_file = "/etc/calciforge/secrets/openai-key"

[[model_shortcuts]]
alias = "sonnet"
model = "anthropic/claude-sonnet-4.6"
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
cargo test -p secrets-client
cargo fmt --all -- --check
cargo clippy --all-targets
```

Install hooks once:

```bash
bash scripts/install-git-hooks.sh
```

## Docs

- [Feature tour and install notes](https://bglusman.github.io/calciforge/)
- [Model gateway RFC](docs/rfcs/model-gateway-primitives.md)
- [Architecture review](docs/architecture-review-2026-04-25.md)
- [Security proxy docs](docs/security-gateway.md)
- [Host-agent docs](crates/host-agent/README.md)
- [Roadmap](docs/roadmap/)

## License

MIT. Some bundled tools, including fnox, carry their own licenses; see
the relevant crate manifests and upstream projects.
