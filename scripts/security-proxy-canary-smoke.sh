#!/usr/bin/env bash
# Smoke-test a live Calciforge security proxy against a prompt-injection canary.
#
# Intended for post-deploy checks on the agent host, where the proxy and CA
# bundle are already installed. Example:
#   PROXY_URL=http://127.0.0.1:8888 \
#   CA_BUNDLE=/root/.config/calciforge/secrets/mitm-ca.pem \
#   scripts/security-proxy-canary-smoke.sh

set -euo pipefail

PROXY_URL="${PROXY_URL:-http://127.0.0.1:8888}"
if [[ -z "${CA_BUNDLE:-}" ]]; then
    service_ca="$(
        systemctl show calciforge-security-proxy -p Environment --value --no-pager 2>/dev/null \
            | tr ' ' '\n' \
            | sed -n 's/^SECURITY_PROXY_CA_CERT=//p' \
            | head -1
    )"
    CA_BUNDLE="${service_ca:-${HOME}/.config/calciforge/secrets/mitm-ca.pem}"
fi
CANARY_URL="${CANARY_URL:-https://ref.jock.pl/modern-web}"

tmp="$(mktemp)"
hdr="$(mktemp)"
cleanup() {
    rm -f "$tmp" "$hdr"
}
trap cleanup EXIT

if [[ ! -s "$CA_BUNDLE" ]]; then
    echo "CA bundle not found: $CA_BUNDLE" >&2
    exit 2
fi

curl -fsS --max-time 5 "${PROXY_URL%/}/health" >/dev/null

status="$(
    curl -sS -L --max-time 30 \
        --proxy "$PROXY_URL" \
        --noproxy "" \
        --cacert "$CA_BUNDLE" \
        -D "$hdr" \
        -o "$tmp" \
        -w "%{http_code}" \
        "$CANARY_URL"
)"

if [[ "$status" != "200" ]]; then
    echo "expected HTTP 200 block page, got $status" >&2
    sed -n '1,40p' "$tmp" >&2 || true
    exit 1
fi

if ! grep -q "Page blocked by Calciforge security gateway" "$tmp"; then
    echo "canary was not replaced by Calciforge block page" >&2
    sed -n '1,80p' "$tmp" >&2 || true
    exit 1
fi

if grep -qi "ignore previous\\|reveal secrets" "$tmp"; then
    echo "blocked response leaked canary prompt-injection content" >&2
    sed -n '1,80p' "$tmp" >&2 || true
    exit 1
fi

echo "security proxy canary blocked: $CANARY_URL via $PROXY_URL"
