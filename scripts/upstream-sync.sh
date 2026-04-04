#!/bin/bash
# upstream-sync.sh — Show upstream zeroclaw commits touching our modified files
# Usage: bash scripts/upstream-sync.sh [since-ref [until-ref]]
#   since-ref: fork point commit (default: aa45c30 = our fork point from v0.3.0)
#   until-ref: upstream target (default: v0.3.2 = latest fetched release tag)
set -euo pipefail

UPSTREAM_DIR="/root/projects/nonzeroclaw"
MONO_DIR="/root/projects/nonzeroclawed"
SINCE="${1:-aa45c30}"     # Our fork point
UNTIL="${2:-v0.3.2}"      # Latest upstream release tag available locally

echo "=== NonZeroClaw — Upstream Sync Status ==="
echo "Fork point:  ${SINCE}"
echo "Upstream:    ${UNTIL}"
echo ""

WATCHED_FILES=(
    "src/gateway/mod.rs"
    "src/agent/loop_.rs"
    "src/gateway/openai_compat.rs"
    "src/providers/anthropic.rs"
    "src/heartbeat/engine.rs"
    "src/channels/mod.rs"
    "src/config/schema.rs"
    "src/providers/mod.rs"
)

echo "=== Upstream commits touching our vendored files (manual backport required) ==="
cd "$UPSTREAM_DIR"
found_any=0
for f in "${WATCHED_FILES[@]}"; do
    commits=$(git log --oneline "${SINCE}..${UNTIL}" -- "$f" 2>/dev/null | wc -l)
    if [ "$commits" -gt 0 ]; then
        found_any=1
        echo ""
        echo "--- $f ($commits commits) ---"
        git log --oneline "${SINCE}..${UNTIL}" -- "$f" 2>/dev/null | head -10
    fi
done
if [ "$found_any" -eq 0 ]; then
    echo "(none — vendored files are up to date with ${UNTIL})"
fi

echo ""
echo "=== Upstream commits NOT touching our vendored files (arrive via cargo update) ==="
cd "$UPSTREAM_DIR"

# Get all commits in range, then filter out those that ONLY touch our watched files
# Simple approximation: show commits that don't touch any watched file
git log --oneline "${SINCE}..${UNTIL}" 2>/dev/null \
    | grep -v "docs\|readme\|README\|ci/\|chore\|sync\|tweet\|release\|bump\|gitignore\|cleanup\|auto-sync\|Merge" \
    | head -20

echo ""
echo "=== Tips ==="
echo "To fetch latest upstream changes:"
echo "  cd ${UPSTREAM_DIR} && git fetch --tags"
echo ""
echo "To extract a patch for a specific file:"
echo "  cd ${UPSTREAM_DIR} && git format-patch ${SINCE}..${UNTIL} -- src/providers/anthropic.rs"
echo ""
echo "Then review and apply to crates/nonzeroclaw/src/providers/anthropic.rs"
