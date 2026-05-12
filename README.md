# Calciforge

> **Keep your castle secure and moving.**

Calciforge is a self-hosted safety layer for AI agents. It sits between
your agents and the outside world, then applies the rules you set:
which model an agent may use, which commands it may run, where a secret
may be sent, and what gets written to the audit trail. The agent can ask
for `{{secret:NAME}}`; it does not need to hold the actual API key.

The longer tour, setup examples, and architecture notes live on
**[calciforge.org](https://calciforge.org/)**.

## What Works Today

This is usable for a solo operator, but still being hardened. Before you
make it daily-driver infrastructure, test it with your real chat
channels, fnox secret store, model providers, and routing choices. Castles
move; config should still have brakes, labels, and fewer mysterious bathroom
potions than Howl would tolerate.

| Area | Status | Where to read more |
|---|---:|---|
| Explicit `{{secret:NAME}}` substitution in URL, headers, and body | Working | [Secret management](https://calciforge.org/#secret-management) |
| Opaque placeholder credentials for supervised agent env vars or managed credential files | Staged primitives | [Placeholder injection mode](https://calciforge.org/roadmap/placeholder-injection-mode.html) |
| Per-secret destination allowlists | Working | [Outbound traffic gating](https://calciforge.org/#outbound-traffic-gating) |
| Local paste form for one-shot and bulk `.env` secret input | Working | [Secret management](https://calciforge.org/#secret-management) |
| Command-line and optional MCP tools for agent-facing secret-name discovery, with no value readback | Working | [Agent-facing tools](https://calciforge.org/#agent-facing-tools-mcp-and-cli) |
| Agent runtime contract for command-line guidance, optional MCP, artifacts, and future Calciforge APIs | Working draft | [Agent runtime contract](docs/agent-runtime-contract.md) |
| Telegram, Matrix, WhatsApp, Signal, and text/iMessage routing | Working | [Multi-channel chat](https://calciforge.org/#multi-channel-chat) |
| OpenAI-compatible model gateway, provider routing, model aliases, alloys, cascades, dispatchers, and local model switching | Working | [Model gateway](docs/model-gateway.md) |
| Helicone-backed gateway observability with dashboard-visible doctor checks | Working | [Model gateway](docs/model-gateway.md#external-gateway-engines) |
| Codex CLI and OpenClaw Codex subscription/OAuth integration paths | Working | [Codex integration](docs/codex-openclaw-integration.md) |
| `calciforge doctor` config/state/endpoint diagnostics | Working | [Quick Start](#quick-start) |
| Default-on inbound prompt-injection scanning, with opt-in outbound exfiltration and response secret-leak heuristics via editable policy | Working | [Traffic gating](https://calciforge.org/#outbound-traffic-gating) |
| Configurable scanner checks with editable Starlark policy, Rust-backed `regex_match`, and optional remote model review | Working | [Security gateway](docs/security-gateway.md) |
| Contributor red-team fixtures for prompt-injection, encoding, Unicode, and tool-policy bypass cases | Working | [Security gateway](docs/security-gateway.md#testing) |
| [`clash`](https://crates.io/crates/clash)-backed tool policy via the `clashd` sidecar | Working | [Policy sidecar](crates/clashd/README.md) |
| mTLS `host-agent` for ZFS, systemd, PCT, git, and exec operations | Working | [Host-agent](crates/host-agent/README.md) |
| Slack/Discord team ChatOps and Castle-to-Castle federation | Roadmap | [Team ChatOps sketch](docs/roadmap/team-chatops-slack-discord.md) |
| Per-agent secret ACLs beyond destination allowlists | Roadmap | [Secret access policy](docs/roadmap/agent-secret-access-policy.md) |

## Quick Start

Current Docker Compose path for a Linux server or LAN staging host:

```bash
git clone https://github.com/bglusman/calciforge
cd calciforge/packaging/docker
cp calciforge.env.example .env
mkdir -p data data-security-proxy data-clashd
openssl rand -base64 32 > data/gateway-api-key
chmod 600 data/gateway-api-key
docker compose --env-file .env build calciforge
docker compose --env-file .env up -d
docker compose --env-file .env exec calciforge \
  calciforge --config /config/config.toml doctor
```

Current macOS release path:

```bash
brew tap bglusman/tap
brew install calciforge
$EDITOR "$(brew --prefix)/etc/calciforge/config.toml"
brew services start calciforge
calciforge doctor
```

Current macOS/source path for development builds and managed-agent wiring:

```bash
git clone https://github.com/bglusman/calciforge
cd calciforge
bash scripts/install.sh
```

See [Packaging and Install Options](docs/packaging.md) for what each path owns.

After install, the default local pieces are:

- `calciforge` — channel router, commands, identity, model gateway
- `security-proxy` on `127.0.0.1:8888` — substitution, destination checks, scanning, credential injection
- `clashd` on `127.0.0.1:9001` — small HTTP adapter around the `clash` policy engine
- `secrets-client` — env → fnox secret resolver
- `calciforge-secrets` — command-line secret-name discovery and `{{secret:NAME}}` reference helper
- `paste-server` — short-lived local/LAN forms for adding secrets without putting values in chat history

The installer attempts to install and initialize `fnox` automatically.
Calciforge uses `~/.config/calciforge` as its app config home by
default; override it with `CALCIFORGE_CONFIG_HOME`. The Calciforge
fnox working directory defaults to the same path (`CALCIFORGE_FNOX_DIR`
can override it), so `cd ~/.config/calciforge && fnox set/list/tui`
manages the same store Calciforge resolves through. On macOS, if no
global fnox provider is configured, the installer adds a
`calciforge-local` Keychain provider under `~/.config/fnox/config.toml`;
on Linux, it creates a local `age` provider backed by an Ed25519 key in
`~/.config/calciforge/secrets/fnox-age-ed25519`. Set
`CALCIFORGE_FNOX_PROVIDER_NAME`, `CALCIFORGE_FNOX_PROVIDER_TYPE`,
`CALCIFORGE_FNOX_AGE_RECIPIENT`, or `FNOX_AGE_KEY_FILE` before install
to bring your own provider/key material. Treat the generated age key as
secret: anyone who can read it can decrypt the local fnox store.

The installer runs full `calciforge doctor` after installing
local services when a config file is present, so cross-node and agent
endpoint checks run by default. Set `CALCIFORGE_INSTALL_DOCTOR_NETWORK=false`
for an offline/local-only install check. Run `calciforge doctor`
again after editing config or moving services. It
validates config, checks referenced secret files without printing
values, flags stale active-agent/model state, detects suspicious
self-routing into the local model gateway, warns if the Calciforge
service itself has ambient proxy env, flags subprocess agents that explicitly
set proxy env, warns about externally managed agent daemons
whose outbound proxy environment cannot be proven,
validates configured scanner policy files and rule syntax, and can probe
configured agent endpoints. Use `--no-network` for a purely local check.

Use `!secret input NAME` or `!secret bulk` to create short-lived paste
links for new secrets. The form stores values in fnox and can attach
destination allowlists so `{{secret:NAME}}` substitution only happens for
approved hosts. Chat-started paste links are intended for browsers on the
same local network unless you configure an authenticated reverse proxy or
tunnel with `CALCIFORGE_PASTE_PUBLIC_BASE_URL`.

Keep Calciforge's own service traffic separate from agent traffic. Point
agents at Calciforge's OpenAI-compatible model gateway for model calls;
that path provides model aliases, alloys, cascades, dispatchers, provider
routing, and observability. Route agent tool/web traffic through
`security-proxy` or a Calciforge fetch/tool integration when returned
content needs scanning or `{{secret:NAME}}` substitution.

First-class adapters are expected to document and test their Calciforge ingress
and egress paths. Generic command-line, generic ACP, and recipe adapters are
useful but best effort unless their recipe proves a network boundary. ACP means
Agent Client Protocol, a way to run persistent agent sessions. Hardened
deployments should disable unverified adapters rather than assuming proxy
environment variables are enough.

For externally managed agent daemons that Calciforge does not launch, proxying
has to be configured on that daemon or its service manager and validated
against `security-proxy` logs:

```bash
export HTTP_PROXY=http://127.0.0.1:8888
export NO_PROXY=localhost,127.0.0.1,::1
```

Do not treat a generic `HTTPS_PROXY` setting as a security boundary unless it
points at Calciforge's inspecting proxy and the agent runtime trusts the
Calciforge CA. CA means certificate authority: a local certificate issuer your
machine can trust so Calciforge may open an HTTPS tunnel, inspect the request,
and then re-encrypt it. Without that trust step, HTTPS is mostly an opaque
tube; Calciforge can see the destination host, not the page or request body
inside. The installer enables the experimental hudsucker-backed listener and
generates a persistent local CA by default; manual deployments can set
`SECURITY_PROXY_CA_CERT=...` and `SECURITY_PROXY_CA_KEY=...`. Use a
Calciforge-owned model gateway, fetch/tool path, audited recipe, or tested
inspecting-proxy setup when HTTPS content needs scanning or secret
substitution.

Secret handling is in a transition period. The working path today is explicit
reference syntax: an agent emits `{{secret:NAME}}`, and Calciforge resolves it
only at an approved destination. The next path is opaque placeholder
credentials: Calciforge will generate fake-looking random values such as
`cfg_OPENAI_API_KEY_<random>`, register them with the security proxy, then
provide them to supervised agents through env vars or managed credential files.
That matters for agents like OpenClaw lanes that expect plaintext credential
files or ordinary `*_API_KEY` variables. Once that path is wired, they can
receive stand-ins instead of real secrets, and the gateway will swap in the
real value only during an allowed outbound request. That path is not the
default yet.

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
- [Agent runtime contract](docs/agent-runtime-contract.md)
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
