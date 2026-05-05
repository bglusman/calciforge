#!/usr/bin/env bash
# Fast validation for packaging templates and operator examples.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

"$ROOT/scripts/render-homebrew-formula.sh" \
    --version 0.1.0-test \
    --base-url https://example.invalid/calciforge \
    --mac-arm64-sha256 0000000000000000000000000000000000000000000000000000000000000000 \
    --mac-intel-sha256 1111111111111111111111111111111111111111111111111111111111111111 \
    --linux-amd64-sha256 2222222222222222222222222222222222222222222222222222222222222222 \
    --output "$TMP/calciforge.rb" >/dev/null

ruby -c "$TMP/calciforge.rb" >/dev/null

if command -v docker >/dev/null 2>&1; then
    if docker compose version >/dev/null 2>&1; then
        docker compose -f "$ROOT/packaging/docker/docker-compose.yml" config >/dev/null
    elif command -v docker-compose >/dev/null 2>&1; then
        docker-compose -f "$ROOT/packaging/docker/docker-compose.yml" config >/dev/null
    else
        echo "docker found but compose plugin not found; skipping compose config check" >&2
    fi
else
    echo "docker not found; skipping compose config check" >&2
fi

echo "packaging checks passed"
