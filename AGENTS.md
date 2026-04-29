# AGENTS.md — Calciforge

Workspace-wide instructions for any AI coding agent (Claude Code, Codex, Copilot cloud agent, OpenClaw, etc.) operating on this repo. Vendor-specific instructions live alongside this file:

- `CLAUDE.md` — Claude Code specifics. **Required reading regardless of agent**: it carries the public-repo secret-discipline rules (never-commit list, two-layer gitleaks, deployment identifiers). Every agent must follow those rules.
- `.github/copilot-instructions.md` — GitHub Copilot PR-review tuning.
- `.github/instructions/rust.instructions.md` — path-scoped (`applyTo: "**/*.rs"`) Rust review specifics; Copilot loads it automatically on Rust diffs.

## What this repo is

Self-hosted security gateway between AI agents and the rest of the world. Multi-crate Rust workspace. Substitutes secrets at the request boundary, gates outbound destinations per-secret, scans inbound + outbound traffic, runs a Starlark policy sidecar (`clashd`), and a separate mTLS daemon (`host-agent`) for sensitive system operations.

User-facing tour: `README.md` → [calciforge.org](https://calciforge.org/).

## Crates (workspace members)

| Crate | Role |
|---|---|
| `calciforge` | Channel router, identity, command dispatch, model gateway. The user-facing binary. |
| `security-proxy` | HTTPS proxy on `127.0.0.1:8888`. Substitutes `{{secret:NAME}}`, gates per-secret destinations, drives scanning. |
| `secrets-client` | `env → fnox → vaultwarden` resolver. Default subprocess wrapper around the `fnox` CLI; opt-in library mode behind `--features fnox-library`. |
| `mcp-server` | MCP surface for agent-facing secret-name discovery. Returns `{{secret:NAME}}` reference tokens; deliberately no `get_secret`. |
| `paste-server` | Localhost-only HTTP form for one-shot / bulk `.env` secret input without putting values in chat history. |
| `clashd` | Daemon adapter around the upstream [`clash`](https://crates.io/crates/clash) Starlark policy crate. The "d" is for daemon. |
| `host-agent` | mTLS RPC server for ZFS / systemd / PCT / git / exec delegation. Has its own `crates/host-agent/AGENTS.md` with security-model specifics. |
| `adversary-detector` | Inbound prompt-injection scanning + outbound exfiltration-pattern scanning. |
| `calciforge-policy-plugin` | Plugin entry point for clashd policy evaluation. |
| `loom-tests` | Concurrency property tests using `loom`. |

## Project vocabulary (don't rename)

- **Calciforge** — the project.
- **Calcifer** — per-agent contract (model, tools, identity, scope).
- **Moving Castle** — a deployment of Calciforge.
- **Doors** — channel/identity entry points (chat channel + identity → routing).
- **`{{secret:NAME}}`** — sentinel string parsed across substitution engine, MCP server, and clashd policies. Don't suggest a typed wrapper; the syntax is a contract.
- **`zeroclaw_*`** — the upstream third-party tool we wrap, NOT pre-rename leftovers from this project.

## Mandatory rules for every agent

1. **Public repo.** Read `CLAUDE.md` before committing. Never commit deployment-specific identifiers (real domains, dynamic-DNS hostnames, private LAN IPs, real chat handles, hardcoded fallback URLs that disclose infra).
2. **Pre-commit gate is real.** It runs `cargo fmt --check`, `cargo clippy -D warnings`, and `gitleaks protect --staged`. Don't bypass with `--no-verify`.
3. **Test fixtures with deliberately-fake secrets** (`+15555550100`, `7000000001`, `eyJ0eXAi…`) are allowlisted in `.gitleaks.toml`. Don't "fix" them.
4. **`{{secret:NAME}}` is a sentinel**, not a placeholder to "improve". Touching its parser without touching every consumer (substitution engine, MCP, clashd policies) is a regression.
5. **Substitution boundary order**: pre-substitution host extraction → URL substitution (gated by per-secret allowlist) → bypass check → header substitution → body substitution → outbound scan. New code must not move bypass before substitution.
6. **No secret values in logs.** Log the secret *name*, never the value. URLs containing bearer tokens or short-lived auth go to `debug!`, not `info!`/`warn!`.
7. **`fnox set <name> <value>` leaks via `ps`/`procfs`.** Use stdin mode (`set <name> -` + write to stdin).
8. **Exec-backed model prompts should travel by stdin or secure temp files.** Avoid putting prompt or secret-bearing text in argv; process listings can expose it on multi-user systems.

## Build / test

```bash
# Workspace-wide
cargo test
cargo build --release
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings

# Per-crate
cargo test -p calciforge
cargo test -p calciforge --features tiktoken-estimator
cargo test -p secrets-client
cargo test -p secrets-client --features fnox-library

# Loom (concurrency model checking)
RUSTFLAGS="--cfg loom" cargo test -p loom-tests --release

# Pre-push gate (run before push)
bash scripts/install-git-hooks.sh   # one-time
```

## Editions

Mixed: older crates on `2021`, newer on `2024`. Known and tracked. Don't bump in a PR that isn't explicitly about edition migration.

## Documentation standard for channels (and future subsystems)

Channel setup guides live in `docs/channels/<channel>.md` and are part of the
public docs site ([calciforge.org/channels/…](https://calciforge.org/channels/telegram)).

**Every channel doc must have:**
- An architecture diagram (ASCII text art showing the message flow)
- Prerequisites section (external accounts, tokens, running services)
- A `[[channels]]` TOML config block — this is the source of truth for config examples
- An identity/routing TOML block showing how to wire users to the channel
- A verify/health-check step

**The TOML blocks are compile-tested.** `crates/calciforge/src/config.rs` contains
`test_channel_docs_<channel>_toml_blocks_valid` tests that load each markdown file
via `include_str!`, extract every fenced `toml` block containing `[[channels]]`,
and parse it against the live `CalciforgeConfig` schema. If a field is renamed or
removed and the doc isn't updated, `cargo test -p calciforge` fails.

**When adding or modifying a channel:**
1. Update or create `docs/channels/<channel>.md` with accurate config examples
2. Add or update the corresponding `test_channel_config_<channel>_inline` and
   `test_channel_docs_<channel>_toml_blocks_valid` tests in `config.rs`
3. Run `cargo test -p calciforge` to confirm all doc tests pass
4. Update `docs/index.md` if adding a new channel

**When renaming a `ChannelConfig` field:**
1. Run `cargo test -p calciforge` — the doc-block tests will fail, naming the broken doc
2. Fix the markdown file, re-run tests, then commit both together

Do not add a new channel without a corresponding `docs/channels/<channel>.md`.

## When working on a specific area, also read

- `crates/host-agent/AGENTS.md` — host-agent security model (Unix-permissions enforcement, fail-closed, mTLS CN→Unix user mapping).
- `docs/rfcs/` — design docs for in-flight subsystems (model gateway primitives, secret-input web UI, etc.).
- `docs/security-gateway.md` — security-proxy internals.
- `docs/model-gateway.md` — Alloy / Cascade / Dispatcher / ExecGateway primitives.
