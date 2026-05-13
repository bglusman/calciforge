#!/usr/bin/env bash
set -euo pipefail

# Provider on_switch hook for Ollama-backed gateway providers.
#
# Calciforge sets:
#   CALCIFORGE_PROVIDER_ID       proxy provider id
#   CALCIFORGE_MODEL_ID          public model ID (for example qwen3.6:27b)
#   CALCIFORGE_UPSTREAM_MODEL_ID upstream ID (for example ollama/qwen3.6:27b)
#   CALCIFORGE_PREV_MODEL_ID     previous public model ID, when known
#
# This script unloads other resident Ollama models before Calciforge sends the
# next gateway request. When CALCIFORGE_OLLAMA_WARMUP is truthy, it also sends
# a tiny non-streaming generation so model load and first-token setup can happen
# before the human-facing request path.

find_ollama() {
    if command -v ollama >/dev/null 2>&1; then
        command -v ollama
        return 0
    fi

    for candidate in \
        /opt/homebrew/bin/ollama \
        /usr/local/bin/ollama \
        /Applications/Ollama.app/Contents/Resources/ollama
    do
        if [[ -x "$candidate" ]]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done

    return 1
}

if [[ -z "${HOME:-}" ]]; then
    user="$(id -un 2>/dev/null || true)"
    if [[ -n "$user" ]]; then
        home_dir="$(
            dscl . -read "/Users/$user" NFSHomeDirectory 2>/dev/null | awk '{print $2}' \
                || awk -F: -v u="$user" '$1 == u {print $6; exit}' /etc/passwd 2>/dev/null \
                || true
        )"
        if [[ -n "$home_dir" && "$home_dir" != "~$user" ]]; then
            export HOME="$home_dir"
        fi
    fi
fi

target="${CALCIFORGE_UPSTREAM_MODEL_ID:-${CALCIFORGE_MODEL_ID:-}}"
target="${target#ollama/}"

if [[ -z "$target" ]]; then
    echo "CALCIFORGE_MODEL_ID or CALCIFORGE_UPSTREAM_MODEL_ID is required" >&2
    exit 64
fi

if ! ollama_bin="$(find_ollama)"; then
    echo "ollama command not found" >&2
    exit 69
fi

current_models="$("$ollama_bin" ps 2>/dev/null | awk 'NR > 1 && $1 != "" {print $1}')"
target_loaded=false

while IFS= read -r model; do
    [[ -n "$model" ]] || continue
    if [[ "$model" == "$target" ]]; then
        target_loaded=true
        continue
    fi
    "$ollama_bin" stop "$model" >/dev/null 2>&1 || true
done <<< "$current_models"

truthy() {
    case "${1:-}" in
        1|true|TRUE|yes|YES|on|ON) return 0 ;;
        *) return 1 ;;
    esac
}

json_escape() {
    if command -v python3 >/dev/null 2>&1; then
        python3 -c 'import json, sys; print(json.dumps(sys.argv[1])[1:-1])' "$1"
    else
        printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
    fi
}

warmup_enabled="${CALCIFORGE_OLLAMA_WARMUP:-true}"
if truthy "$warmup_enabled" && [[ "$target_loaded" != true ]]; then
    warmup_required="${CALCIFORGE_OLLAMA_WARMUP_REQUIRED:-false}"
    if command -v curl >/dev/null 2>&1; then
        host="${OLLAMA_HOST:-http://127.0.0.1:11434}"
        host="${host%/}"
        keep_alive="${CALCIFORGE_OLLAMA_KEEP_ALIVE:-24h}"
        warmup_timeout="${CALCIFORGE_OLLAMA_WARMUP_TIMEOUT_SECONDS:-120}"
        warmup_ctx="${CALCIFORGE_OLLAMA_WARMUP_CONTEXT:-1024}"
        payload="$(printf \
            '{"model":"%s","prompt":"Reply with exactly: ready","stream":false,"keep_alive":"%s","options":{"num_ctx":%s}}\n' \
            "$(json_escape "$target")" \
            "$(json_escape "$keep_alive")" \
            "$warmup_ctx")"
        if ! curl -fsS --max-time "$warmup_timeout" \
            -H 'Content-Type: application/json' \
            -d "$payload" \
            "$host/api/generate" >/dev/null; then
            if truthy "$warmup_required"; then
                exit 1
            fi
            echo "warning: Ollama warmup failed for $target; continuing to gateway request" >&2
        fi
    else
        if ! "$ollama_bin" run "$target" "Reply with exactly: ready" >/dev/null; then
            if truthy "$warmup_required"; then
                exit 1
            fi
            echo "warning: Ollama warmup failed for $target; continuing to gateway request" >&2
        fi
    fi
fi
