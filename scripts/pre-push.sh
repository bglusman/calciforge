#!/usr/bin/env bash
# Pre-push checks for zeroclawed. Run before every push.
# Usage: bash scripts/pre-push.sh [--quick] [--loom-only]
#
# --quick      Skip loom and slow tests
# --loom-only  Only run loom tests
#
# LESSONS LEARNED (why each check exists):
# 1. cargo fmt catches import ordering — rustfmt sorts imports
#    alphabetically. CI will fail if you forget to run it.
# 2. cargo clippy -D warnings — CI runs with -D warnings, so
#    any clippy warning becomes a hard error.
# 3. Loom MUST run in an isolated crate (loom-tests), NOT inside
#    zeroclawed. `--cfg loom` globally disables tokio::net, which
#    cascades to break hyper-util and the entire dependency graph.
#    Always: `cargo test -p loom-tests` with RUSTFLAGS="--cfg loom".
#    Never: `cargo test -p zeroclawed --cfg loom`.
# 4. loom::Arc doesn't impl Copy — must clone before second use.
# 5. loom has no yield_now() — use thread::yield_now() instead.
# 6. Workspace integrity — removing a crate from Cargo.toml
#    [workspace] members breaks `cargo test --workspace`.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

pass() { echo -e "${GREEN}✓ $1${NC}"; }
fail() { echo -e "${RED}✗ $1${NC}"; FAILURES=$((FAILURES+1)); }
warn() { echo -e "${YELLOW}⚠ $1${NC}"; }

FAILURES=0
QUICK=false
LOOM_ONLY=false

for arg in "$@"; do
  case $arg in
    --quick) QUICK=true ;;
    --loom-only) LOOM_ONLY=true ;;
  esac
done

# ─── Format check ───────────────────────────────────────────────
if [ "$LOOM_ONLY" = false ]; then
  echo "── Formatting ─────────────────────────────────────────"
  if cargo fmt --all -- --check 2>&1; then
    pass "fmt clean"
  else
    fail "fmt issues — run: cargo fmt --all"
  fi
fi

# ─── Clippy ─────────────────────────────────────────────────────
if [ "$LOOM_ONLY" = false ]; then
  echo "── Clippy ─────────────────────────────────────────────"
  if cargo clippy --workspace --all-targets -- -D warnings 2>&1; then
    pass "clippy clean"
  else
    fail "clippy warnings"
  fi
fi

# ─── Unit tests ─────────────────────────────────────────────────
if [ "$QUICK" = false ] && [ "$LOOM_ONLY" = false ]; then
  echo "── Unit tests ─────────────────────────────────────────"
  if cargo test --workspace --exclude loom-tests 2>&1; then
    pass "unit tests"
  else
    fail "unit tests"
  fi
fi

# ─── Loom tests (isolated crate) ───────────────────────────────
if [ "$QUICK" = false ]; then
  echo "── Loom tests ─────────────────────────────────────────"
  # CRITICAL: `RUSTFLAGS="--cfg loom"` is a GLOBAL flag that affects every crate.
  # It disables tokio::net (Loom can't simulate TCP), which breaks hyper-util,
  # which breaks the entire zeroclawed dependency graph. Solution: an isolated
  # crate (loom-tests) with minimal deps — only `loom` itself.
  #
  # NEVER run: cargo test -p zeroclawed --cfg loom  <-- BROKEN
  # ALWAYS run: cargo test -p loom-tests with RUSTFLAGS="--cfg loom"
  if LOOM_MAX_PREEMPTIONS=2 RUSTFLAGS="--cfg loom" cargo test -p loom-tests 2>&1; then
    pass "loom tests (6 tests)"
  else
    fail "loom tests"
  fi
fi

# ─── Workspace integrity ───────────────────────────────────────
if [ "$LOOM_ONLY" = false ]; then
  echo "── Workspace check ────────────────────────────────────"
  # Verify loom-tests is in workspace members
  if grep -q '"crates/loom-tests"' Cargo.toml; then
    pass "loom-tests in workspace members"
  else
    fail "loom-tests missing from Cargo.toml [workspace] members"
  fi
  # Verify no cfg(loom) in main crate (breaks tokio::net)
  if grep -rq '#\[cfg(loom)\]' crates/zeroclawed/src/ 2>/dev/null; then
    warn "cfg(loom) found in zeroclawed/src — inert but ideally move to crates/loom-tests/"
  fi
fi

# ─── Summary ────────────────────────────────────────────────────
echo ""
if [ $FAILURES -eq 0 ]; then
  pass "All checks passed — safe to push"
  exit 0
else
  echo -e "${RED}✗ $FAILURES check(s) failed — fix before pushing${NC}"
  exit 1
fi
