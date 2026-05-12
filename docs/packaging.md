---
layout: default
title: Packaging and Install Options
---

# Packaging and Install Options

Status: Implemented for Docker Compose, Homebrew, source installs, and release
archives; managed-agent wiring remains adapter-specific.

Calciforge supports four install shapes. They serve different audiences and
should not be mixed up in docs or support notes.

## Start Here

- **Linux server or LAN staging host:** use Docker Compose. It is the fastest
  packaged path today and does not require Rust on the host.
- **macOS release install:** use Homebrew. It installs published release
  binaries and runs Calciforge under Homebrew service supervision.
- **Development or managed-agent wiring:** use the source installer. It builds
  from the checkout and can configure first-class agents, certificates, and
  local service supervision.
- **Manual/offline installs:** use release archives when you need to place the
  binaries yourself without Homebrew or Compose.

## Docker Compose

Use this for trials, LAN staging, and repeatable smoke environments:

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
  calciforge --config /config/config.toml doctor --no-network
```

The Compose example runs Calciforge, `security-proxy`, and `clashd` from the
same image. It is separate from `scripts/docker-compose.yml`, which remains the
CI/mock-LLM smoke stack. Release Compose installs should use the published GHCR
image. GitHub Actions publishes `ghcr.io/bglusman/calciforge:main` and
`ghcr.io/bglusman/calciforge:sha-<commit>` on every merge to `main`; release
tags also publish immutable version tags from the release workflow. Use `:main`
for staging nodes that should follow the integration branch, and pin a version
or SHA tag for stable hosts. Local `--build` remains useful for development and
pre-release smoke tests.

To validate the packaged Compose path without real provider credentials:

```bash
scripts/packaging-docker-smoke.sh
```

That smoke script overlays the packaged Compose file with a mock
OpenAI-compatible backend and checks Calciforge, `security-proxy`, `clashd`,
model listing, and a chat completion.

For CLI-backed agents in Compose, install or mount the agent binaries inside
the Calciforge container. Host-level tools are not visible to Docker by
default. `doctor` checks configured subprocess commands such as `acpx`,
`opencode`, `codex`, and `claude` from the same runtime that will dispatch chat
messages.

For clean reinstall testing, inspect the reset plan before removing anything:

```bash
scripts/clean-install-reset.sh
scripts/clean-install-reset.sh --include-docker --include-config
scripts/clean-install-reset.sh --ssh root@calciforge-staging.example --include-docker
```

The reset helper is dry-run by default. Add `--execute` only after the printed
plan matches the machine you intend to clean. Config/state removal, Docker
cleanup, and Calciforge-managed agent services are separate flags so a service
reset does not silently delete operator data.

Fnox is treated as a dependency, not Calciforge-owned state. The reset helper
will not remove `~/.config/fnox` unless you pass `--include-fnox`, because that
vault may already contain unrelated or sensitive operator data.

## Homebrew Binary Formula

Use this for normal macOS installs from published release archives. The formula
installs released binaries; it does not build from source.

```bash
brew tap bglusman/tap
brew install calciforge
$EDITOR "$(brew --prefix)/etc/calciforge/config.toml"
brew services start calciforge
calciforge doctor
```

Packaging maintainers render the formula from:

```bash
scripts/render-homebrew-formula.sh --help
```

Docker, Homebrew, and release archives share the runtime binary contract in
`packaging/runtime-binaries.txt`. `scripts/check-packaging.sh` fails if Docker,
the archive builder, or the rendered Homebrew formula drifts from that list.

This is a binary packaging path with Homebrew service supervision and a
Homebrew `fnox` dependency for secret helpers. It expects you to provide config at
`$(brew --prefix)/etc/calciforge/config.toml`; it does not discover agents,
install certificates, or populate secrets by itself.

Use the formula when you want released binaries and native macOS supervision.
Run the explicit installer/configuration flow after installing when Calciforge
should discover agents, install certificates, bootstrap secrets, or wire remote
nodes.

## Source Installer

Use this when developing Calciforge, testing current `main`, or wiring managed
agents:

```bash
git clone https://github.com/bglusman/calciforge
cd calciforge
bash scripts/install.sh
```

This path builds from source, manages launchd/systemd services, configures local
state, and can wire supported agents. It requires Rust.

Agent instruction files are prompt surface, so the installer does not silently
patch arbitrary repos. Use `--agent-instructions-print` to review the default
block, `--agent-instructions-file PATH` to patch a specific file, or
`--agent-workspace PATH` to patch `PATH/AGENTS.md`. The managed block teaches the
agent how to use `{{secret:NAME}}` references, `calciforge-secrets`, and
Calciforge API/MCP surfaces without asking the user to paste plaintext secrets
into chat.

In multi-node installs, the secret store remains central on the Calciforge host.
Managed agent hosts should get an API-backed `calciforge-secrets` wrapper that
talks back to Calciforge; they should not get their own independent fnox vault by
default. The Rust `calciforge install` subcommand installs that wrapper only when
given `--agent-helper-base-url`; it smoke-tests the helper from the agent host
and warns if `~/.local/bin` is not visible in that runtime's `PATH`.

Do not deploy fnox to arbitrary agent or sidecar nodes just to make helper
commands pass. A remote `security-proxy` that performs secret substitution still
needs an explicit central-secret-backend mode or co-location with the Calciforge
secret owner; otherwise it would become a second source of truth.

## Choosing an Install Shape

Most users should start with a single-node install: Homebrew on macOS, Docker
Compose on a server, or the source installer when they are developing or need
managed local agents. In this mode Calciforge, `security-proxy`, `clashd`, and
the model gateway live on one host.

Multi-node installs are explicit. Keep one Calciforge control host as the owner
of config, policy, model gateway, and secrets. Remote agent hosts should point
back to that control host through managed wrappers, API-backed
`calciforge-secrets`, or gateway/proxy endpoints. Do not create independent fnox
vaults on every agent host unless the operator intentionally wants separate
secret ownership.

Containerized or virtualized agents are the recommended isolation boundary for
stronger deployments. Expose only the Calciforge-approved gateway, proxy, and
helper endpoints into those containers or VMs.

## Secret Path Validation

Packaging smoke tests intentionally avoid real secrets. Before tagging a release,
also run at least one staging test with a real agent and a real fnox-backed
secret:

1. Store a secret in the central Calciforge/fnox store through Calciforge or an
   API-backed `calciforge-secrets` helper.
2. Ask the agent to use only the `{{secret:NAME}}` reference.
3. Confirm the agent never sees the plaintext secret.
4. Confirm the model request, security proxy decision, and destination-scoped
   substitution all succeed.

When an agent needs to generate a new credential, prefer a pipe into fnox or the
Calciforge secrets CLI so the agent can hand off generated bytes without
receiving or echoing a durable secret value in chat history.

## Manual Release Archives

Release operators can build tarballs with:

```bash
scripts/build-dist-archive.sh
```

The archive layout is the same one consumed by the Homebrew formula template:
core binaries under `bin/`, plus license/readme metadata.

Tag releases (`v*`) also run the release packaging workflow. That workflow
builds Linux and macOS archives, uploads them to the GitHub release, and pushes
the Docker image to GHCR. Manual workflow runs can build the same artifacts
without pushing an image unless `push_image` is enabled.
