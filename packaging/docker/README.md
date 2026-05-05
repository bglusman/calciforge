# Docker Packaging

This Compose example is for trials, LAN staging, and operators who want to run
Calciforge without installing Rust locally.

From this directory:

```bash
cp calciforge.env.example .env
mkdir -p data data-security-proxy data-clashd
docker compose --env-file .env up --build
```

The example starts:

- `calciforge` on `${CALCIFORGE_PROXY_PORT:-18792}`
- `security-proxy` on `${CALCIFORGE_SECURITY_PROXY_PORT:-8888}`
- `clashd` on `${CALCIFORGE_CLASHD_PORT:-9001}`

The Compose file builds the shared `calciforge:local` image through the
`calciforge` service and reuses that image for the sidecars. This avoids
building the same Rust image three times with older `docker-compose` versions.

The default Calciforge config points the model gateway at an OpenAI-compatible
service on the host machine at `http://host.docker.internal:11434/v1`, which
matches common Ollama-compatible local testing. Edit `config.example.toml` or set
`CALCIFORGE_CONFIG` before using it for real traffic.

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
