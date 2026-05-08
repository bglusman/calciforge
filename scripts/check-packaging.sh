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

installer_shell_files=("$ROOT/scripts/install.sh" "$ROOT"/scripts/lib/*.sh)
for shell_file in "${installer_shell_files[@]}"; do
    bash -n "$shell_file"
done

bash -s -- "$ROOT" <<'BASH'
set -euo pipefail

ROOT="$1"

source "$ROOT/scripts/lib/common.sh"
truthy yes
! truthy false
[[ "$(expand_home_path "~/calciforge")" == "$HOME/calciforge" ]] || {
    echo "expand_home_path did not expand ~/ paths" >&2
    exit 1
}
[[ "$(toml_basic_string $'a"b\tc')" == '"a\"b\tc"' ]] || {
    echo "toml_basic_string did not escape quotes and tabs" >&2
    exit 1
}

ask_install() { return 1; }
CALCIFORGE_CONFIG_HOME="$HOME/.config/calciforge"
CALCIFORGE_FNOX_DIR="$CALCIFORGE_CONFIG_HOME"
CALCIFORGE_FNOX_PROVIDER_NAME="calciforge-local"
CALCIFORGE_FNOX_PROVIDER_TYPE=""
CALCIFORGE_FNOX_WARMUP=false
CALCIFORGE_FNOX_AGE_RECIPIENT=""
CONFIGURE_ONLY=false
FNOX_AGE_KEY_FILE=""
IS_ROOT=false
PLATFORM=Linux
source "$ROOT/scripts/lib/fnox.sh"
[[ "$(default_fnox_provider_type)" == "age" ]] || {
    echo "default fnox provider type on Linux should be age" >&2
    exit 1
}
PLATFORM=Darwin
[[ "$(default_fnox_provider_type)" == "keychain" ]] || {
    echo "default fnox provider type on Darwin should be keychain" >&2
    exit 1
}
CALCIFORGE_FNOX_PROVIDER_TYPE="custom"
[[ "$(default_fnox_provider_type)" == "custom" ]] || {
    echo "explicit fnox provider type should override platform default" >&2
    exit 1
}

fake_bin="$(mktemp -d)"
fake_home="$(mktemp -d)"
trap 'rm -rf "$fake_bin" "$fake_home"' EXIT
cat >"$fake_bin/brew" <<'SH'
#!/usr/bin/env bash
exit 7
SH
chmod +x "$fake_bin/brew"
PATH="$fake_bin:/usr/bin:/bin"
HOME="$fake_home"
CALCIFORGE_CONFIG_HOME="$HOME/.config/calciforge"
CALCIFORGE_FNOX_DIR="$CALCIFORGE_CONFIG_HOME"
CALCIFORGE_FNOX_PROVIDER_TYPE=""
CONFIGURE_ONLY=false
PLATFORM=Darwin
ask_install() { return 0; }
set +e
ensure_fnox >/dev/null 2>&1
fnox_rc=$?
case "$-" in
    *e*)
        echo "ensure_fnox must preserve disabled errexit when sourced" >&2
        exit 1
        ;;
esac
set -e
[[ "$fnox_rc" -eq 1 ]] || {
    echo "fake brew fallback should fail without enabling errexit" >&2
    exit 1
}
BASH

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
