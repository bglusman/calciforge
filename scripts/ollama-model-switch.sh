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
# next gateway request. The request itself will load the target model.

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
if [[ -z "$current_models" ]]; then
    exit 0
fi

while IFS= read -r model; do
    [[ -n "$model" ]] || continue
    if [[ "$model" == "$target" ]]; then
        continue
    fi
    "$ollama_bin" stop "$model" >/dev/null 2>&1 || true
done <<< "$current_models"
