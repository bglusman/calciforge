#!/bin/sh
# Example Calciforge CLI-agent wrapper for Codex CLI subscription/OAuth access.
#
# This is intentionally small and black-box oriented: Calciforge passes the
# rendered prompt on stdin, and the Codex CLI owns authentication.
# Validate flags against your installed Codex version before production use.

set -eu

model="${CALCIFORGE_CODEX_MODEL:-gpt-5.5}"

if ! command -v codex >/dev/null 2>&1; then
  echo "codex executable not found in PATH" >&2
  exit 127
fi

exec codex exec --color never --sandbox read-only --skip-git-repo-check -m "$model" -
