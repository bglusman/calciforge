#!/bin/sh
# Example Calciforge CLI-agent wrapper for Kimi CLI subscription access.
#
# Kimi CLI flags and subscription/API-key behavior can change. Treat this as a
# starting point, not a compatibility guarantee; test with your installed
# version and account. The prompt stays on stdin so it is not exposed in argv.

set -eu

if command -v kimi >/dev/null 2>&1; then
  kimi_bin="kimi"
elif [ -x "$HOME/.local/bin/kimi" ]; then
  kimi_bin="$HOME/.local/bin/kimi"
else
  echo "kimi executable not found in PATH or ~/.local/bin" >&2
  exit 127
fi

set -- --quiet

if [ -n "${CALCIFORGE_KIMI_MODEL:-}" ]; then
  set -- "$@" --model "$CALCIFORGE_KIMI_MODEL"
fi

case "${CALCIFORGE_KIMI_THINKING:-default}" in
  true|1|yes|on)
    set -- "$@" --thinking
    ;;
  false|0|no|off)
    set -- "$@" --no-thinking
    ;;
  default|"")
    ;;
  *)
    echo "invalid CALCIFORGE_KIMI_THINKING value: $CALCIFORGE_KIMI_THINKING" >&2
    exit 2
    ;;
esac

exec "$kimi_bin" "$@"
