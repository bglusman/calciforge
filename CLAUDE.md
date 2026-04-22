# CLAUDE.md — Instructions for Claude Code (and other AI agents) on this repo

This file is loaded automatically by Claude Code when working in this
repository. Other agents: please honor the same rules when operating here.

## This repo is PUBLIC

The repository is public. Anything committed here can be read by anyone,
including git history. Plan accordingly.

## Never commit these to this repo

Even as defaults, examples, docstrings, or comments:

- **API tokens, passwords, session keys, Bearer tokens** — even ones
  labeled "test" or "demo". If a scanner would match the pattern, do not
  paste it.
- **Deployment-specific infrastructure identifiers.** Categories to avoid:
  - Personal domains (any domain the maintainer owns, plus all subdomains)
  - Dynamic-DNS hostnames (e.g., any `*.<some-ddns-service>`) — reveals
    router vendor and home IP when resolved
  - Private-LAN IP addresses (192.168.*, 10.*, 100.64.0.0/10 CGNAT)
  - Real chat identifiers (Matrix handles, Discord user-ids, Telegram
    chat ids) tied to specific users
  - Private-deployment model names that don't exist outside the maintainer's
    environment
- **Vault / credential-store URLs** pointing at specific instances.
- **Hard-coded fallback URLs** that would disclose infrastructure if the
  environment variable is unset. Env vars should be mandatory — no
  "helpful" default that reveals the production URL.

Specific *examples* of domains/handles/IPs that would trip these rules
are NOT named in this file, because doing so would itself be disclosure.
They live in `.gitleaks.local.toml` (gitignored); see
`.gitleaks.local.toml.example` for the template.

## What to use instead

- Env vars with no default, or with RFC-documented placeholders
  (`https://vault.example.com`, `192.0.2.1`, etc.)
- Fixtures under `tests/**/fixtures/` for test data
- Files named `*.example.*` for sample configs users copy-then-edit

## Two-layer scanner (gitleaks)

- **`.gitleaks.toml`** — public, generic rules. Catches categorical leaks
  (private IP ranges, Bearer-in-header, URL basic-auth, plus gitleaks'
  built-in token patterns). Ships in the repo; enforced in CI.
- **`.gitleaks.local.toml`** — gitignored, per-deployment. Holds the
  maintainer's specific domains, handles, and internal identifiers.
  CI does NOT run this file; it's a tighter local check for developers.
  Copy from `.gitleaks.local.toml.example` to get started.

## Pre-push local check

```
brew install gitleaks                                  # or apt/download
gitleaks detect --source . --config .gitleaks.toml --verbose
# Optional, if you've populated a local-only config:
gitleaks detect --source . --config .gitleaks.local.toml --verbose
```

Exit 0 = safe. Any finding → fix before commit. A CI failure on
`secret-scan` will block the PR merge.

## If you think a match is a false positive

1. Rework the code/doc to avoid the pattern (preferred).
2. If the value is RFC-reserved or otherwise truly safe, add a tight
   allowlist entry to `.gitleaks.toml` with a comment explaining why.

Do NOT bypass the scan with `--no-verify` or `git commit -n` — the CI
check will still catch it.

## Scope of these instructions

Apply to everything committed to this repo:
- Source code, config, docs, commit messages, issue/PR bodies
- Tests and fixtures (intentional fakes allowed, under the allowlisted paths)
- Example configs (placeholder values only, never real ones)

## Related docs

- `AGENTS.md` — host-agent project-specific build/architecture rules
- `README.md` — user-facing project docs
- `.gitleaks.local.toml.example` — how to extend gitleaks with your own
  per-deployment patterns
