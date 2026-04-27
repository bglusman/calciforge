# Copilot review instructions for Calciforge

**Review philosophy: if uncertain, do not comment.** A clean review with no spurious findings beats one that catches a real issue buried under noise. Verify every claim against the diff AND the existing codebase before posting — do not flag missing tests, handlers, imports, or behavior that already exists elsewhere in the PR or repo.

## What this repo is

Self-hosted security gateway between AI agents and the rest of the world. Multi-crate Rust workspace. Substitutes secrets at the request boundary, gates outbound destinations per-secret, runs a Starlark policy sidecar (`clashd` → wraps the upstream `clash` crate), separate mTLS daemon (`host-agent`) for sensitive system operations. Path-scoped Rust review specifics live in [`.github/instructions/rust.instructions.md`](instructions/rust.instructions.md). Repo-wide rules also in [`AGENTS.md`](../AGENTS.md) and [`CLAUDE.md`](../CLAUDE.md) — the latter's "never commit these" list is exactly the kind of leakage Copilot should flag.

## Priority order

1. **Correctness** — does the code do what its name/comments/tests claim?
2. **Security** — secret leakage in logs, missing auth/identity check, substitution-boundary regression, exfil paths
3. **Resource & error handling** — see Rust path-scoped instructions
4. **Performance** — only when measurable in a hot path; otherwise skip
5. **Style** — skip entirely (pre-commit handles it)

## Tools that already gate — don't re-flag what they catch

- `cargo fmt --all -- --check` — formatting (pre-commit blocks commit)
- `cargo clippy --all-targets -- -D warnings` — lints (pre-commit + CI)
- `gitleaks protect --staged` — secret scanning (pre-commit) + the same scan in CI. `.gitleaks.toml` allowlists by **path** (`tests/**/fixtures/`, `docs/rfcs/*.md`, lockfiles, `crates/paste-server/src/lib.rs`, `*.example.*`) and by **regex** (loopback IPs, RFC 5737 doc-ranges, a few specific inherited-from-main values). If a finding is inside an allowlisted path or matches an allowlisted regex, do NOT re-flag it — it's intentional. Findings outside the allowlist are real.

## Project context — NOT bugs despite looking like them

- `{{secret:NAME}}` is a sentinel string parsed across the substitution engine, MCP server, and clashd policies. Do NOT suggest a typed wrapper; changing the syntax breaks the integration contract.
- `FnoxClient` (in `crates/secrets-client`) is intentionally a subprocess wrapper around the `fnox` CLI. Library mode (`FnoxLibrary`) is opt-in behind the `fnox-library` cargo feature. Do NOT suggest "use the library" — the trade-off is documented in `fnox_library.rs` module doc.
- `clashd` is a daemon adapter around the upstream `clash` crate. The "d" is for "daemon". Do NOT suggest renaming or merging into `security-proxy`.
- References to `zeroclaw_*` (no trailing `ed`) are the upstream third-party tool we wrap, NOT pre-rename leftover of this project.
- Mixed Rust edition (2021 + 2024) is known and tracked. Do NOT suggest the bump unless the PR is explicitly about edition migration.

## Self-discipline

- Do NOT repeat a comment already made on a parent or sibling PR in the same stack. If the same observation was raised on PR #N and merged/addressed, do not re-raise on PR #N+1. Past noisy patterns: the dead-doc-reference comment was posted four times across PRs #20/#23/#25; the env-mutex/`serial_test` comment was posted eight+ times across #19/#22/#23.
- Do NOT repeat the same finding across multiple files in one PR. Post once on the highest-leverage file with "same applies to: foo.rs:N, bar.rs:M".
- Do NOT post PR-description-vs-diff mismatches as inline comments. Use the overall-review comment slot if anywhere.
- "Consider adding tests" without naming the specific assertion or bug class is filler. Skip it.
- Do NOT flag intentional default changes ("default mitm=false") as if they need release notes — the title IS the release note.

## Past bug classes worth keeping an eye on

These are real regressions caught here before. Look for the pattern, not the literal code:

1. **Substitution moved after bypass-return** — must be substitution → bypass → forward, never the reverse.
2. **`resolve_and_substitute(url, None)`** for URL substitution — defeats the per-secret destination allowlist for URL-embedded secrets. Always extract `dest_host` from the pre-substitution URL.
3. **URL with bearer/short-lived token logged at `info!`/`warn!`** — drop to `debug!`, or log only the host/path without the token.
4. **`fnox set <name> <value>` with value as argv** — leaks via `ps`/`procfs`. Use stdin mode (`set <name> -` + write value to stdin).

## Style of feedback

Concise, file:line references, one issue per comment, show literal `before → after` if suggesting a change. Skip preamble.
