#!/usr/bin/env bash
# Smoke-test a live Calciforge instance through the mock channel API.
#
# The mock channel routes through the same command handler and adapter router as
# production channels, but stays localhost-only and deterministic enough for
# post-deploy checks. Enable it in config with:
#
# [[channels]]
# kind = "mock"
# enabled = true
# control_port = 9090

set -euo pipefail

MOCK_URL="${MOCK_URL:-http://127.0.0.1:9090}"
SENDER="${SENDER:-brian}"
AGENT="${AGENT:-hermes}"
PROMPT="${PROMPT:-Reply with exactly: calciforge-smoke-ok}"
EXPECTED="${EXPECTED:-calciforge-smoke-ok}"

need() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "$1 is required" >&2
        exit 2
    }
}

need curl
need python3

post_message() {
    local text="$1"
    python3 - "$MOCK_URL" "$SENDER" "$text" <<'PYEOF'
import json
import sys
import urllib.request

url, sender, text = sys.argv[1:4]
body = json.dumps({"sender": sender, "text": text}).encode()
req = urllib.request.Request(
    url.rstrip("/") + "/send",
    data=body,
    headers={"content-type": "application/json"},
    method="POST",
)
with urllib.request.urlopen(req, timeout=120) as resp:
    payload = json.loads(resp.read().decode())
if not payload.get("success"):
    raise SystemExit(f"mock send failed: {payload}")
print(payload.get("data", {}).get("response", ""))
PYEOF
}

curl -fsS --max-time 5 "${MOCK_URL%/}/health" >/dev/null

agents="$(post_message "!agents")"
if ! grep -Eq "^[0-9]+\\. ${AGENT}: \`!agent switch ${AGENT}\`" <<<"$agents"; then
    echo "agent '$AGENT' is not available to sender '$SENDER'" >&2
    echo "$agents" >&2
    exit 1
fi

switch_reply="$(post_message "!agent switch ${AGENT}")"
if ! grep -q "Switched to ${AGENT}" <<<"$switch_reply"; then
    echo "agent switch failed" >&2
    echo "$switch_reply" >&2
    exit 1
fi

status="$(post_message "!status")"
if ! grep -q "active agent: ${AGENT}" <<<"$status"; then
    echo "status did not report active agent '$AGENT'" >&2
    echo "$status" >&2
    exit 1
fi

reply="$(post_message "$PROMPT")"
if [[ "$(tr -d '\r\n' <<<"$reply")" != "$EXPECTED" ]]; then
    echo "unexpected agent reply; expected '$EXPECTED'" >&2
    echo "$reply" >&2
    exit 1
fi

echo "mock channel smoke passed: sender=$SENDER agent=$AGENT"
