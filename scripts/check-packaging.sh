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
ruby -e 'require "yaml"; ARGV.each { |path| YAML.load_file(path) }' \
    "$ROOT/.github/workflows/release-packaging.yml"

if "$ROOT/scripts/render-homebrew-formula.sh" --version 2>"$TMP/missing-arg.err"; then
    echo "render-homebrew-formula accepted missing flag value" >&2
    exit 1
fi
grep -q "missing value for --version" "$TMP/missing-arg.err"

if grep -Eq '^[[:space:]]*dist/?[[:space:]]*$' "$ROOT/.dockerignore"; then
    echo ".dockerignore must not use bare 'dist' or 'dist/'; it excludes nested plugin dist assets required by clean Docker builds" >&2
    exit 1
fi
grep -Eq '^[[:space:]]*/dist/?[[:space:]]*$' "$ROOT/.dockerignore"
test -s "$ROOT/crates/calciforge-policy-plugin/dist/index.js"

python3 - "$ROOT/scripts/install.sh" "$ROOT/docs/model-gateway.md" <<'PY'
import pathlib
import re
import sys

install = pathlib.Path(sys.argv[1]).read_text()
docs = pathlib.Path(sys.argv[2]).read_text()

if 'strip_model_prefix = "opencode-zen/"' not in docs:
    raise SystemExit("docs/model-gateway.md must document the public OpenCode Zen selector prefix")

call = re.search(
    r'_ensure_opencode_provider\s*\\\n'
    r'\s*"\$ZC_CONFIG"\s*\\\n'
    r'\s*"opencode-zen"\s*\\\n'
    r'\s*"https://opencode\.ai/zen/v1"\s*\\\n'
    r'\s*"([^"]+)"\s*\\',
    install,
)
if not call:
    raise SystemExit("could not find OpenCode Zen provider installer call")

if call.group(1) != "opencode-zen/":
    raise SystemExit(
        "OpenCode Zen installer selector prefix must stay opencode-zen/ "
        f"(got {call.group(1)!r})"
    )
PY

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
