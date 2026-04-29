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

## Documentation standard (gold standard — applies to all crates)

**The rule:** every user-facing config section, public API, and user-facing feature must
have documentation with examples that are verified by the test suite. Docs that aren't
tested go stale silently; tested docs can't.

Channels (`docs/channels/`) were the first area to reach this standard and serve as the
reference implementation. All other config sections and crates follow the same pattern.

### What needs docs

- **Every `pub struct` in `config.rs`** — all fields documented; a TOML example that
  parses against the live schema and is tested
- **Every user-facing feature** — setup guide in `docs/` covering prerequisites,
  config, identity wiring (where applicable), and a verify step
- **Every public API in library crates** — at least one doctest per public function /
  type showing the happy path; complex types get a full usage example

### Two testing mechanisms — use the right one for the crate type

**Library crates** (`secrets-client`, `adversary-detector`, `clashd`, `mcp-server`,
`paste-server`, `security-proxy`): use native Rust **doctests** in `///` comments.
`cargo test --doc -p <crate>` compiles and runs them. If an API changes and the
example is wrong, the build fails.

```rust
/// Fetches a secret value by name.
///
/// ```no_run
/// # use secrets_client::FnoxClient;
/// # #[tokio::main] async fn main() -> anyhow::Result<()> {
/// let client = FnoxClient::new();
/// let value = client.get("MY_API_KEY").await?;
/// # Ok(())
/// # }
/// ```
pub async fn get(&self, name: &str) -> Result<String, FnoxError> { ... }
```

**Binary crates** (`calciforge`, `host-agent`): use **`include_str!` + unit tests**
in the relevant source file's `#[cfg(test)]` block. The test loads the markdown doc,
extracts fenced TOML blocks, and parses them against the live config schema.

```rust
#[test]
fn test_agent_docs_toml_blocks_valid() {
    let doc = include_str!("../../../docs/agents.md");
    for block in doc_blocks_with(doc, "[[agents]]") {
        parse_as_config(&block);  // panics with file + block on failure
    }
}
```

The channel tests in `config.rs` (`test_channel_docs_*_toml_blocks_valid`) are the
canonical example — copy that pattern for each new config section.

### Doc file locations

| What | Location | Tested by |
|---|---|---|
| Channel setup guides | `docs/channels/<channel>.md` | `config.rs` unit tests |
| Agent config | `docs/agents.md` | `config.rs` unit tests |
| Routing / identity | `docs/routing.md` | `config.rs` unit tests |
| Model gateway (alloys, cascades, dispatchers, exec models) | `docs/model-gateway.md` | `config.rs` unit tests |
| Security / proxy config | `docs/security-gateway.md` | `config.rs` unit tests |
| Library crate APIs | `///` doc comments in source | `cargo test --doc -p <crate>` |
| host-agent setup | `docs/host-agent.md` | `host-agent` unit tests |

### Structure for setup guides in `docs/`

Every setup guide covering a user-facing feature must have these sections, in order:

1. **Architecture** — ASCII diagram showing data flow end to end
2. **Prerequisites** — external accounts, tokens, running services, dependencies
3. **Config** — TOML block(s) with every required field shown; optional fields in a table
4. **Identity / routing** (where applicable) — how to wire a user to the feature
5. **Verify** — commands to confirm it's working (health check, log output, smoke test)

### Rules for every agent and contributor

**When adding a config field to any `pub struct`:**
1. Add a `///` doc comment to the field explaining what it does and its default
2. Update the TOML example in the relevant `docs/` file
3. Run `cargo test -p calciforge` — the doc-block tests will fail if the example is now
   invalid, telling you exactly which file to fix
4. Commit the code change and the doc update together — never in separate PRs

**When adding a new config section:**
1. Create `docs/<section>.md` with all five structure sections above
2. Add `test_<section>_docs_toml_blocks_valid` in the relevant `#[cfg(test)]` block
3. Link from `docs/index.md`

**When adding a public function to a library crate:**
1. Write at least one `///` doctest showing the happy path
2. Run `cargo test --doc -p <crate>` to confirm it compiles and passes
3. For functions with meaningful error paths, add a second doctest for the failure case

**When renaming any field or function:**
- `cargo test` will name every broken doc example
- Fix markdown files and doctests in the same commit as the rename

Do not add a new channel, config section, or public library API without corresponding
tested documentation. A PR that adds functionality without docs should not be merged.

## When working on a specific area, also read

- `crates/host-agent/AGENTS.md` — host-agent security model (Unix-permissions enforcement, fail-closed, mTLS CN→Unix user mapping).
- `docs/rfcs/` — design docs for in-flight subsystems (model gateway primitives, secret-input web UI, etc.).
- `docs/security-gateway.md` — security-proxy internals.
- `docs/model-gateway.md` — Alloy / Cascade / Dispatcher / ExecGateway primitives.
