#!/usr/bin/env bash
# Smoke-test configured Calciforge model-gateway provider routes.
#
# This intentionally uses Calciforge's OpenAI-compatible gateway endpoint, not
# the upstream provider URL. It verifies client auth, provider route matching,
# model prefix rewrites, provider API key files, and the gateway backend path.

set -euo pipefail

BASE_URL="${CALCIFORGE_GATEWAY_BASE_URL:-http://127.0.0.1:18083}"
API_KEY_FILE="${CALCIFORGE_GATEWAY_API_KEY_FILE:-}"
AGENT_ID="${CALCIFORGE_GATEWAY_SMOKE_AGENT_ID:-gateway}"
EXPECTED="${CALCIFORGE_GATEWAY_SMOKE_EXPECTED:-calciforge-provider-smoke}"
TIMEOUT="${CALCIFORGE_GATEWAY_SMOKE_TIMEOUT:-60}"
MODELS=()

usage() {
    cat <<'EOF'
Usage: scripts/model-gateway-provider-smoke.sh --api-key-file PATH [options] MODEL...

Options:
  --base-url URL        Calciforge gateway base URL. Default: http://127.0.0.1:18083
  --api-key-file PATH   File containing the Calciforge gateway client API key.
  --agent-id ID         x-agent-id header to send. Default: gateway
  --expected TEXT       Exact expected assistant content. Default: calciforge-provider-smoke
  --timeout SECONDS     Per-request curl timeout. Default: 60

Environment equivalents:
  CALCIFORGE_GATEWAY_BASE_URL
  CALCIFORGE_GATEWAY_API_KEY_FILE
  CALCIFORGE_GATEWAY_SMOKE_AGENT_ID
  CALCIFORGE_GATEWAY_SMOKE_EXPECTED
  CALCIFORGE_GATEWAY_SMOKE_TIMEOUT
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --base-url)
            BASE_URL="${2:-}"
            shift 2
            ;;
        --api-key-file)
            API_KEY_FILE="${2:-}"
            shift 2
            ;;
        --agent-id)
            AGENT_ID="${2:-}"
            shift 2
            ;;
        --expected)
            EXPECTED="${2:-}"
            shift 2
            ;;
        --timeout)
            TIMEOUT="${2:-}"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        --)
            shift
            MODELS+=("$@")
            break
            ;;
        -*)
            printf 'error: unknown option: %s\n\n' "$1" >&2
            usage >&2
            exit 2
            ;;
        *)
            MODELS+=("$1")
            shift
            ;;
    esac
done

if [[ -z "$API_KEY_FILE" ]]; then
    printf 'error: --api-key-file is required\n\n' >&2
    usage >&2
    exit 2
fi

if [[ ! -r "$API_KEY_FILE" ]]; then
    printf 'error: API key file is not readable: %s\n' "$API_KEY_FILE" >&2
    exit 2
fi

if [[ ${#MODELS[@]} -eq 0 ]]; then
    printf 'error: at least one model ID is required\n\n' >&2
    usage >&2
    exit 2
fi

if ! command -v curl >/dev/null 2>&1; then
    printf 'error: curl is required\n' >&2
    exit 2
fi

if ! command -v python3 >/dev/null 2>&1; then
    printf 'error: python3 is required\n' >&2
    exit 2
fi

API_KEY="$(tr -d '\n' < "$API_KEY_FILE")"
if [[ -z "$API_KEY" ]]; then
    printf 'error: API key file is empty: %s\n' "$API_KEY_FILE" >&2
    exit 2
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT
curl_config="$tmp_dir/curl-headers.conf"
chmod 700 "$tmp_dir"
python3 - "$curl_config" "$API_KEY" "$AGENT_ID" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
headers = [
    f"Authorization: Bearer {sys.argv[2]}",
    f"x-agent-id: {sys.argv[3]}",
    "Content-Type: application/json",
]
path.write_text("".join(f"header = {json.dumps(header)}\n" for header in headers))
path.chmod(0o600)
PY

BASE_URL="${BASE_URL%/}"
failures=0

for model in "${MODELS[@]}"; do
    payload="$(
        python3 - "$model" "$EXPECTED" <<'PY'
import json
import sys

model, expected = sys.argv[1], sys.argv[2]
print(json.dumps({
    "model": model,
    "messages": [{"role": "user", "content": f"Reply exactly: {expected}"}],
    "max_tokens": 16,
}))
PY
    )"

    response="$(
        curl -sS -m "$TIMEOUT" -w '\nHTTP_STATUS:%{http_code}\n' \
            --config "$curl_config" \
            "${BASE_URL}/v1/chat/completions" \
            --data "$payload" 2>&1 || true
    )"
    response="${response//$API_KEY/<redacted>}"
    status="${response##*HTTP_STATUS:}"
    status="${status//$'\n'/}"
    body="${response%$'\n'HTTP_STATUS:*}"

    content="$(
        printf '%s' "$body" | python3 -c '
import json
import sys

data = json.load(sys.stdin)
print(data["choices"][0]["message"].get("content", ""))
' 2>/dev/null || true
    )"

    if [[ "$status" == "200" && "$content" == "$EXPECTED" ]]; then
        printf 'ok: %s returned expected content\n' "$model"
    else
        failures=$((failures + 1))
        printf 'error: %s failed smoke: status=%s content=%q\n' "$model" "$status" "$content" >&2
        if [[ -z "$content" ]]; then
            printf '%s\n' "$body" | sed -E 's/[A-Za-z0-9_-]{32,}/<redacted>/g' >&2
        fi
    fi
done

if [[ "$failures" -gt 0 ]]; then
    printf '\nmodel gateway provider smoke failed with %s failure(s)\n' "$failures" >&2
    exit 1
fi

printf '\nmodel gateway provider smoke passed for %s model(s)\n' "${#MODELS[@]}"
