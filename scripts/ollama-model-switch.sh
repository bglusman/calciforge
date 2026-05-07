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

target="${CALCIFORGE_UPSTREAM_MODEL_ID:-${CALCIFORGE_MODEL_ID:-}}"
target="${target#ollama/}"

if [[ -z "$target" ]]; then
    echo "CALCIFORGE_MODEL_ID or CALCIFORGE_UPSTREAM_MODEL_ID is required" >&2
    exit 64
fi

if ! command -v ollama >/dev/null 2>&1; then
    echo "ollama command not found" >&2
    exit 69
fi

current_models="$(ollama ps 2>/dev/null | awk 'NR > 1 && $1 != "" {print $1}')"
if [[ -z "$current_models" ]]; then
    exit 0
fi

while IFS= read -r model; do
    [[ -n "$model" ]] || continue
    if [[ "$model" == "$target" ]]; then
        continue
    fi
    ollama stop "$model" >/dev/null 2>&1 || true
done <<< "$current_models"
