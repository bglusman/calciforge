#!/usr/bin/env bash
# Manual Docker smoke checks for candidate agent recipes/orchestrators.
#
# This intentionally verifies only installability and scriptable CLI surfaces.
# Provider authentication, model execution, and workspace mutation remain
# operator-reviewed integration steps.

set -euo pipefail

if ! command -v docker >/dev/null 2>&1; then
  echo "docker is required for agent recipe smoke checks" >&2
  exit 1
fi

run_smoke() {
  local name="$1"
  shift
  echo "==> $name"
  "$@"
  echo
}

run_smoke "npcsh CLI" \
  docker run --rm \
    -e PIP_DISABLE_PIP_VERSION_CHECK=1 \
    python:3.12-slim \
    sh -lc "python -m pip install --no-cache-dir 'npcsh[lite]' >/tmp/npcsh-install.log 2>&1 && command -v npcsh && command -v npc && npc --help >/tmp/npc-help.txt && sed -n '1,24p' /tmp/npc-help.txt"

run_smoke "Oh My OpenAgent / oh-my-opencode CLI" \
  docker run --rm \
    -e OMO_SEND_ANONYMOUS_TELEMETRY=0 \
    -e OMO_DISABLE_POSTHOG=1 \
    oven/bun:1 \
    sh -lc "bunx --yes oh-my-opencode --help >/tmp/omo-help.txt && bunx --yes oh-my-opencode run --help >/tmp/omo-run-help.txt && sed -n '1,24p' /tmp/omo-run-help.txt"

run_smoke "Gas Town CLI" \
  docker run --rm \
    node:22-slim \
    sh -lc "npm install -g @gastown/gt >/tmp/gt-install.log 2>&1 && command -v gt && gt --help >/tmp/gt-help.txt && sed -n '1,36p' /tmp/gt-help.txt"

echo "agent recipe smoke checks passed"
