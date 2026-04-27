#!/bin/sh
# Example Calciforge exec-model wrapper for Claude CLI subscription access.
#
# Claude CLI flags and subscription terms can change. Treat this as a starting
# point, not a compatibility guarantee; test with your installed version.
# The prompt is intentionally left on stdin so it is not exposed in argv.

set -eu

if ! command -v claude >/dev/null 2>&1; then
  echo "claude executable not found in PATH" >&2
  exit 127
fi

if [ -n "${CALCIFORGE_CLAUDE_MODEL:-}" ]; then
  exec claude -p --model "$CALCIFORGE_CLAUDE_MODEL"
fi

exec claude -p
