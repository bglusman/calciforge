#!/usr/bin/env bash
set -euo pipefail

mode="${1:-pr}"

case "$mode" in
  pr)
    export PROPTEST_CASES="${PROPTEST_CASES:-128}"
    ;;
  nightly)
    export PROPTEST_CASES="${PROPTEST_CASES:-2048}"
    export PROPTEST_MAX_SHRINK_ITERS="${PROPTEST_MAX_SHRINK_ITERS:-4096}"
    ;;
  *)
    echo "usage: $0 [pr|nightly]" >&2
    exit 2
    ;;
esac

run() {
  if [[ -n "${GITHUB_ACTIONS:-}" ]]; then
    echo "::group::$*"
  else
    printf '\n==> %s\n' "$*"
  fi

  "$@"

  if [[ -n "${GITHUB_ACTIONS:-}" ]]; then
    echo "::endgroup::"
  fi
}

ensure_uv_for_hegel() {
  if command -v uv >/dev/null 2>&1; then
    return
  fi

  echo "uv is required for Hegel; installing uv"
  if command -v pipx >/dev/null 2>&1; then
    pipx install uv
  else
    python3 -m pip install --user --break-system-packages uv
  fi
  export PATH="$HOME/.local/bin:$PATH"

  if [[ -n "${GITHUB_PATH:-}" ]]; then
    echo "$HOME/.local/bin" >> "$GITHUB_PATH"
  fi

  command -v uv >/dev/null 2>&1
}

echo "boundary aggression mode: $mode"
echo "PROPTEST_CASES=$PROPTEST_CASES"

run cargo test -p calciforge proxy::openai_streaming::tests -- --nocapture
run cargo test -p calciforge proxy::routing::tests -- --nocapture
run cargo test -p calciforge proxy::auth::tests -- --nocapture
run cargo test -p calciforge adapters::openclaw_channel::openclaw_channel_reply_tests -- --nocapture
run cargo test -p calciforge --test e2e property_tests -- --nocapture

if [[ "$mode" == "nightly" ]]; then
  ensure_uv_for_hegel
  run cargo test -p calciforge --features hegel install:: -- --nocapture
  run cargo test -p calciforge adapters:: -- --nocapture
  run cargo test -p calciforge channels:: -- --nocapture
  run cargo test -p calciforge config:: -- --nocapture
  run cargo test -p calciforge doctor:: -- --nocapture
  run cargo test -p host-agent -- --nocapture
  run cargo test -p secrets-client -- --nocapture
  run cargo test -p security-proxy -- --nocapture
  run cargo test -p adversary-detector -- --nocapture
fi

run cargo build -p calciforge
export CALCIFORGE_BIN="${CALCIFORGE_BIN:-target/debug/calciforge}"
run python3 scripts/model-gateway-helicone-smoke.py
