# Copilot review instructions for Calciforge

These instructions tune GitHub Copilot's PR-review behavior for this
repo. Repo conventions live in [`AGENTS.md`](../AGENTS.md) (host-agent
coding standards) and [`CLAUDE.md`](../CLAUDE.md) (public-repo secret
discipline) — Copilot should follow both. This file adds review-time
priorities, what to skip, and past-mistake context.

## What this repo is

Calciforge is a self-hosted security gateway between AI agents and the
rest of the world. It substitutes secrets at the request boundary,
gates outbound destinations per-secret, scans inbound + outbound
traffic, runs a Starlark policy sidecar, and a separate mTLS daemon
for sensitive system ops. Multi-crate Rust workspace; primary
language is Rust (edition 2021 in older crates, 2024 in newer — known
inconsistency, on the roadmap).

## Tools that already gate — skip these in review

A pre-commit gate runs on every commit and blocks merge if any fails:

- `cargo fmt --all -- --check` — never suggest formatting changes
- `cargo clippy --all-targets -- -D warnings` — never suggest lint fixes Copilot would normally flag (unless you spot a *new* clippy lint added since the last clippy run)
- `gitleaks protect --staged` — never flag secrets in staged content; if you see a secret-shaped string it's either intentional in a test fixture (gitleaks-allowlisted in `.gitleaks.toml`) or real-and-already-blocked

Don't waste a review comment on anything in these classes.

## What to prioritize

This is a security-critical codebase. Weight findings in this order.

**HIGH (block merge unless addressed):**

- **Secret leakage in logs** — any `tracing::*!` / `eprintln!` that interpolates a value with provenance from an `env::var`, `vault::get_secret`, `FnoxClient`, header value, request body, or anything named `*_key`/`*_token`/`*_secret`. Logs should record the *name* of the secret, never the value.
- **Substitution-boundary correctness** — anything that touches `crates/security-proxy/src/{proxy,substitution,router}.rs` should be checked against the model in [`docs/architecture-review-2026-04-25.md`](../docs/architecture-review-2026-04-25.md). The order is: pre-substitution host extraction → URL substitution (gated by per-secret allowlist) → bypass check → header substitution → body substitution → outbound scan. New code must not move bypass before substitution.
- **`unwrap()` / `expect()` in non-test code** — flag with `?` or `anyhow::Context` suggestion. Tests are exempt.
- **Missing `unsafe` block around `std::env::set_var` / `remove_var`** — required in edition 2024. We have a `crate::env` shim in `crates/calciforge`; new code should use that.
- **Blocking I/O in async context** — `std::fs::*` / `std::process::Command` / `reqwest::blocking::*` inside an `async fn` or `tokio::spawn`.
- **Auth bypass paths** — anything routing decisions in `crates/security-proxy` or `crates/host-agent` that doesn't check identity / token / mTLS cert before acting.

**MEDIUM:**

- Race conditions in `Arc<Mutex<…>>` patterns — could a channel be cleaner / could `tokio::sync::RwLock` reduce contention.
- Error swallowing — `let _ = …` on a fallible call, `.ok()` discarding context, `match … { Err(_) => return Default::default() }` patterns.
- Per-call regex compile (`Regex::new` inside a hot fn) — should be `LazyLock<Regex>` at module scope.
- `String` allocations in hot paths where `Cow<str>` or `&str` would work.
- Missing `kill_on_drop(true)` on `tokio::process::Command` for subprocess wrappers.

**LOW:** doc-comment improvements, naming nits, `.clone()` where `Arc::clone` is more conventional.

## Project-specific context that's NOT bugs

Reviewers commonly miss these. Don't flag them as issues:

- **`{{secret:NAME}}` is a sentinel string** by design. Don't suggest "use a typed wrapper" — the substitution engine matches on this exact literal and downstream tools (clashd policies, MCP server) parse it. Changing the syntax breaks the integration contract.
- **Test fixtures intentionally contain fake-shaped secrets** like `+15555550100`, `7000000001`, `sk-test-…`, `eyJ0eXAiOiJKV1Q…`. These are RFC-style placeholders or post-history-scrub fakes. The `.gitleaks.toml` allowlist is the source of truth for what's intentional.
- **`FnoxClient` is a subprocess wrapper around the `fnox` CLI by design.** Library mode (`FnoxLibrary`, behind the `fnox-library` cargo feature) is opt-in; subprocess is the default. Don't suggest "rewrite to use the library" — the trade-off is documented in `crates/secrets-client/src/fnox_library.rs`'s module doc.
- **clashd is a thin daemon adapter around the upstream `clash` policy crate.** It's named `clashd` because it's a daemon. Don't suggest renaming or merging it into security-proxy — they're deliberately separate processes.
- **`zeroclaw_*` references in code are the upstream third-party tool we wrap**, NOT leftover from the (pre-rename) project name. Don't suggest renaming those.
- **Mixed Rust edition** (2021 in older crates, 2024 in newer) is known and tracked. Don't suggest the bump in PRs that aren't about edition migration.

## Past-mistake patterns to specifically check for

Real bugs that landed and were caught in later review — Copilot should look for these classes:

1. **Substitution moved after bypass-return** — bypassed requests would forward literal `{{secret:NAME}}` text. Order must be substitution → bypass → forward.
2. **`substitute_url(url, None)` passing `None` for `dest_host`** — defeats the per-secret destination allowlist for URL-embedded secrets. Always extract dest_host before substituting.
3. **Bearer URL logged at `info!` level** — anything calling `info!` with a URL that may contain a query-param token, header, or short-lived bearer should drop to `debug!` or strip the sensitive parts.
4. **`fnox set <name> <value>` with value as argv** — leaks via `ps`/`/proc`. Use stdin-mode (`set <name> -` + write value to stdin) instead.
5. **Default-bind to `0.0.0.0`** for new HTTP services — should be `127.0.0.1` unless the service genuinely needs LAN exposure (Origin check + auth required if so).
6. **Hardcoded fallback URLs** when an env var is unset — exposes deployment-specific infrastructure to the public repo. See CLAUDE.md "Hard-coded fallback URLs" rule.

## What to skip even if technically correct

- "Consider adding tests" without specifying *what* the test would assert and *what bug* it would catch.
- Renaming suggestions where existing names match crate-naming convention (functional names like `security-proxy`, `host-agent`, `secrets-client` — not project-prefixed).
- Doc-comment additions where the existing comment is accurate and the function is short / self-explanatory.
- Feature-creep proposals ("you should also support X") — keep review focused on the diff.

## Style of feedback

Concise, file:line references, one issue per comment. If suggesting a change, show the literal `before → after`. Skip preamble.
