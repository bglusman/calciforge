#!/usr/bin/env bash
# scripts/lib/common.sh — Shared shell primitives for installer modules.
#
# Ownership:
#   Keep broadly reusable, dependency-light installer helpers here. Domain
#   behavior belongs in a focused module such as fnox.sh or helicone.sh.
#
# Required globals:
#   None.
#
# Optional globals:
#   HOME — used by expand_home_path.

[[ -n "${_CALCIFORGE_COMMON_LIB_LOADED:-}" ]] && return 0
_CALCIFORGE_COMMON_LIB_LOADED=1

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
CYAN='\033[0;36m'
NC='\033[0m'

ok()   { echo -e "${GREEN}✓${NC} $*"; }
warn() { echo -e "${YELLOW}!${NC} $*"; }
die()  { echo -e "${RED}✗${NC} $*" >&2; exit 1; }
hdr()  { echo -e "\n${CYAN}━━ $* ━━${NC}"; }

truthy() {
    case "${1:-}" in
        1|true|TRUE|yes|YES|on|ON) return 0 ;;
        *) return 1 ;;
    esac
}

expand_home_path() {
    local path="$1"
    case "$path" in
        "~") printf '%s\n' "$HOME" ;;
        "~/"*) printf '%s/%s\n' "$HOME" "${path#\~/}" ;;
        *) printf '%s\n' "$path" ;;
    esac
}

random_hex() {
    local bytes="$1" label="${2:-random value}"
    if command -v openssl >/dev/null 2>&1; then
        openssl rand -hex "$bytes"
        return 0
    fi
    if command -v python3 >/dev/null 2>&1; then
        python3 - "$bytes" <<'PY'
import secrets
import sys

print(secrets.token_hex(int(sys.argv[1])))
PY
        return 0
    fi
    die "openssl or python3 is required to generate ${label}"
}

toml_basic_string() {
    local value="$1"
    value="${value//\\/\\\\}"
    value="${value//\"/\\\"}"
    value="${value//$'\n'/\\n}"
    value="${value//$'\r'/\\r}"
    value="${value//$'\t'/\\t}"
    printf '"%s"\n' "$value"
}
