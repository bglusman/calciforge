#!/usr/bin/env bash
# pre-commit-hook.sh — mechanical gate run before every commit.
#
# Blocks the commit on:
#   - rustfmt violation (fast)
#   - clippy warning/error (incremental, usually fast after warm cache)
#   - gitleaks finding in the staged changes
#
# Explicitly NOT included:
#   - cargo test — too slow for every commit; runs in pre-push instead
#   - any Claude call — semantic review is in the post-commit hook
#
# Install:
#   ln -s ../../scripts/pre-commit-hook.sh .git/hooks/pre-commit
#   (or use scripts/install-git-hooks.sh)

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

fail() { echo -e "${RED}✗ pre-commit:${NC} $*" >&2; exit 1; }
note() { echo -e "${YELLOW}…${NC} $*"; }
ok()   { echo -e "${GREEN}✓${NC} $*"; }

# Only run the rust checks if the commit touches Rust files. Docs /
# config / scripts commits shouldn't pay the clippy tax.
STAGED_RUST="$(git diff --cached --name-only --diff-filter=ACM | grep -E '\.rs$|Cargo\.(toml|lock)$' || true)"

if [[ -n "$STAGED_RUST" ]]; then
    note "rustfmt check..."
    if ! ~/.cargo/bin/cargo fmt --all -- --check >/dev/null 2>&1; then
        ~/.cargo/bin/cargo fmt --all -- --check 2>&1 | head -20
        fail "rustfmt would reformat — run \`cargo fmt --all\` and re-stage"
    fi
    ok "rustfmt clean"

    note "clippy (workspace, -D warnings)..."
    # Workspace scope on purpose — clippy in a single crate misses
    # integration issues. Incremental build keeps repeat runs fast.
    if ! ~/.cargo/bin/cargo clippy --workspace --all-targets --quiet -- -D warnings 2>&1 | tail -20; then
        fail "clippy found warnings — fix or `#[allow]` with a reason"
    fi
    ok "clippy clean"
else
    note "no Rust changes — skipping fmt/clippy"
fi

# Gitleaks always runs, regardless of file type, because the most
# dangerous leaks (API keys in docs, hardcoded URLs in installer
# scripts) aren't in .rs files.
note "gitleaks (staged only)..."
if ! command -v gitleaks >/dev/null 2>&1; then
    if [[ -x /opt/homebrew/bin/gitleaks ]]; then
        GITLEAKS=/opt/homebrew/bin/gitleaks
    elif [[ -x /usr/local/bin/gitleaks ]]; then
        GITLEAKS=/usr/local/bin/gitleaks
    else
        note "gitleaks not installed — skipping (install: brew install gitleaks)"
        GITLEAKS=""
    fi
else
    GITLEAKS="$(command -v gitleaks)"
fi

if [[ -n "$GITLEAKS" ]]; then
    if ! "$GITLEAKS" protect --staged --config .gitleaks.toml >/dev/null 2>&1; then
        "$GITLEAKS" protect --staged --config .gitleaks.toml 2>&1 | tail -20
        fail "gitleaks found a secret-shaped pattern in staged changes"
    fi
    ok "gitleaks clean"
fi

ok "pre-commit gate passed"
