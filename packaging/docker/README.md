# Docker Packaging

This Compose example is for trials, LAN staging, and operators who want to run
Calciforge without installing Rust locally.

From this directory:

```bash
cp calciforge.env.example .env
mkdir -p data data-security-proxy data-clashd
docker compose --env-file .env build calciforge
docker compose --env-file .env up -d
```

For staging or release installs, set `CALCIFORGE_IMAGE` in `.env` to a
published GHCR image before running `up -d`. `:main` follows every merge to the
main branch, `sha-<commit>` pins an immutable commit image, and release version
tags are published by the release workflow.

When migrating an existing systemd or Homebrew install to Compose, stop the
existing service before binding the same ports. Preserve config paths that are
already referenced from `config.toml`: if the file points at
`/etc/calciforge/secrets/...` or `/etc/calciforge/whatsapp/session.db`, mount
the host config directory at `/etc/calciforge` in the container and run
Calciforge with `--config /etc/calciforge/config.toml`, or rewrite those paths
to the Compose mounts before starting the container. The sample Compose file
uses `/config` for clean trials; live migrations should keep paths stable unless
they are intentionally changing layout.

The example starts:

- `calciforge` on `${CALCIFORGE_PROXY_PORT:-18792}`
- `security-proxy` on `${CALCIFORGE_SECURITY_PROXY_PORT:-8888}`
- `clashd` on `${CALCIFORGE_CLASHD_PORT:-9001}`

The Compose file builds the shared `calciforge:local` image through the
`calciforge` service and reuses that image for the sidecars. Build the
`calciforge` service before the first `up`; otherwise Compose may try to pull
the sidecar image before the local image exists. The split build also avoids
building the same Rust image three times with older `docker-compose` versions.
The Dockerfile defaults to `CALCIFORGE_DOCKER_BUILD_JOBS=1` to avoid OOM kills on
small staging hosts; increase it only on builders with enough RAM.

The default Calciforge config points the model gateway at an OpenAI-compatible
service on the host machine at `http://host.docker.internal:11434/v1`, which
matches common Ollama-compatible local testing. Edit `config.example.toml` or set
`CALCIFORGE_CONFIG` before using it for real traffic.

When validating a fresh install on a staging host, first run the repository
reset helper in dry-run mode from the repo root:

```bash
scripts/clean-install-reset.sh --include-docker --include-config
scripts/clean-install-reset.sh --ssh root@calciforge-staging.example --include-docker
```

Add `--execute` only after the printed plan matches the host you intend to
clean.

Fnox state is intentionally not part of `--include-config`. Calciforge relies on
fnox, but it may not be the only thing using the local vault, and the vault may
contain unrelated sensitive data. Use `--include-fnox` only when the fnox config
and vault are dedicated to this install.

For a provider-free smoke test of this packaged Compose runtime, run from the
repository root:

```bash
scripts/packaging-docker-smoke.sh
```

The smoke script adds `docker-compose.smoke.yml`, points Calciforge at
`config.smoke.toml`, and verifies the three packaged services plus one mock chat
completion.

This smoke does not prove that a real agent has loaded Calciforge instructions
or can use fnox-backed secrets. Run a separate staging test with
`--agent-instructions-print`, `--agent-instructions-file PATH`, or
`--agent-workspace PATH`, then verify the agent uses `{{secret:NAME}}` references
rather than plaintext secrets.

The security proxy mounts both `security-proxy.example.toml` and
`agents.example.json`. The TOML file controls proxy behavior and MITM CA paths;
the JSON file is the legacy credential-injection provider map. Edit those files
or set `CALCIFORGE_SECURITY_PROXY_CONFIG` / `CALCIFORGE_AGENTS_CONFIG` when
testing provider credential injection.

`clashd` mounts the same agent JSON plus `policy.example.star`. Edit
`policy.example.star` or set `CALCIFORGE_CLASHD_POLICY` when testing stricter
tool-call policy behavior.

Each service has a separate writable data mount. In particular, do not share the
security proxy data directory with other containers: it is mounted at
`/var/lib/calciforge` inside the proxy container because that is the proxy's
default CA path, and it contains the generated MITM CA private key when you do
not provide one explicitly.

This is not yet the hardened production isolation story. It is a repeatable
packaged runtime for smoke tests and local/LAN experiments. For production-like
security validation, keep using the staging matrix and explicit proxy canary
tests.
