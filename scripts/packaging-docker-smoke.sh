#!/usr/bin/env bash
# Smoke-test the packaged Docker Compose runtime with a mock OpenAI-compatible
# backend. This validates packaging/docker instead of the separate CI stack.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
COMPOSE_DIR="$ROOT/packaging/docker"
PROJECT_NAME="${CALCIFORGE_DOCKER_SMOKE_PROJECT:-calciforge-packaging-smoke}"
ARTIFACT_DIR="${CALCIFORGE_STAGING_ARTIFACT_DIR:-$ROOT/.tmp/packaging-docker-smoke}"
SUMMARY="$ARTIFACT_DIR/summary.jsonl"
LOG_FILE="$ARTIFACT_DIR/docker-compose.log"

mkdir -p "$ARTIFACT_DIR"
: > "$SUMMARY"

compose_cmd() {
    if docker compose version >/dev/null 2>&1; then
        docker compose --project-name "$PROJECT_NAME" "$@"
    elif command -v docker-compose >/dev/null 2>&1; then
        docker-compose --project-name "$PROJECT_NAME" "$@"
    else
        echo "docker compose or docker-compose is required" >&2
        return 127
    fi
}

build_image() {
    docker build \
        --tag "${CALCIFORGE_IMAGE:-calciforge:local}" \
        --file "$ROOT/crates/calciforge/Dockerfile" \
        --build-arg "CARGO_BUILD_JOBS=${CALCIFORGE_DOCKER_BUILD_JOBS:-1}" \
        --build-arg "CARGO_BUILD_PROFILE=${CALCIFORGE_DOCKER_BUILD_PROFILE:-docker}" \
        "$ROOT"
}

record() {
    python3 - "$1" "$2" "$3" <<'PY' >> "$SUMMARY"
import json
import sys

print(json.dumps({"check": sys.argv[1], "status": sys.argv[2], "detail": sys.argv[3]}))
PY
    printf '%-32s %-6s %s\n' "$1" "$2" "$3"
}

failures=0
run_check() {
    local check="$1"
    shift

    local output
    if output="$("$@" 2>&1)"; then
        record "$check" "pass" "$output"
    else
        failures=$((failures + 1))
        record "$check" "fail" "$output"
    fi
}

wait_for_health() {
    local url="$1"
    local name="$2"
    local attempts="${3:-60}"

    for _ in $(seq 1 "$attempts"); do
        if curl -fsS --max-time 3 "$url" >/dev/null 2>&1; then
            echo "$name healthy"
            return 0
        fi
        sleep 2
    done

    echo "$name did not become healthy at $url"
    return 1
}

cleanup() {
    compose_cmd \
        -f "$COMPOSE_DIR/docker-compose.yml" \
        -f "$COMPOSE_DIR/docker-compose.smoke.yml" \
        logs > "$LOG_FILE" 2>&1 || true
    compose_cmd \
        -f "$COMPOSE_DIR/docker-compose.yml" \
        -f "$COMPOSE_DIR/docker-compose.smoke.yml" \
        down -v >/dev/null 2>&1 || true
}
trap cleanup EXIT

export CALCIFORGE_CONFIG="${CALCIFORGE_CONFIG:-./config.smoke.toml}"
export CALCIFORGE_PROXY_PORT="${CALCIFORGE_PROXY_PORT:-28792}"
export CALCIFORGE_SECURITY_PROXY_PORT="${CALCIFORGE_SECURITY_PROXY_PORT:-28888}"
export CALCIFORGE_CLASHD_PORT="${CALCIFORGE_CLASHD_PORT:-29001}"

cd "$COMPOSE_DIR"

compose_cmd -f docker-compose.yml -f docker-compose.smoke.yml down -v >/dev/null 2>&1 || true
if [[ "${CALCIFORGE_SKIP_DOCKER_BUILD:-0}" == "1" ]]; then
    echo "Skipping Docker image build because CALCIFORGE_SKIP_DOCKER_BUILD=1"
else
    build_image
fi
compose_cmd -f docker-compose.yml -f docker-compose.smoke.yml up -d

run_check "calciforge health" wait_for_health "http://127.0.0.1:$CALCIFORGE_PROXY_PORT/health" "calciforge"
run_check "security-proxy health" wait_for_health "http://127.0.0.1:$CALCIFORGE_SECURITY_PROXY_PORT/health" "security-proxy"
run_check "clashd health" wait_for_health "http://127.0.0.1:$CALCIFORGE_CLASHD_PORT/health" "clashd"

run_check "packaged model list" bash -c "
    set -euo pipefail
    body=\"\$(curl -fsS --max-time 10 http://127.0.0.1:$CALCIFORGE_PROXY_PORT/v1/models)\"
    printf '%s' \"\$body\" | grep -q '\"gpt-4\"'
    echo 'gpt-4 present'
"

run_check "packaged chat completion" bash -c "
    set -euo pipefail
    body=\"\$(curl -fsS --max-time 20 \
        -H 'Content-Type: application/json' \
        -d '{\"model\":\"gpt-4\",\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]}' \
        http://127.0.0.1:$CALCIFORGE_PROXY_PORT/v1/chat/completions)\"
    content=\"\$(BODY=\"\$body\" python3 - <<'PY'
import json
import os

data = json.loads(os.environ['BODY'])
print(data['choices'][0]['message'].get('content') or '')
PY
)\"
    if [[ \"\$content\" != 'Hello!' ]]; then
        printf 'unexpected chat completion body: %s\n' \"\$body\" >&2
        exit 1
    fi
    echo 'mock completion returned'
"

echo "Summary written to $SUMMARY"
echo "Logs will be written to $LOG_FILE during cleanup"

if [[ "$failures" -ne 0 ]]; then
    echo "$failures packaged Docker smoke check(s) failed" >&2
    exit 1
fi
