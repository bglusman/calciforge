#!/usr/bin/env bash
set -euo pipefail

duration="${1:-3600}"
surface="${2:-all}"

case "$duration" in
  one-hour) duration=3600 ;;
  day) duration=86400 ;;
  three-days) duration=259200 ;;
esac

if ! [[ "$duration" =~ ^[0-9]+$ ]] || [[ "$duration" -le 0 ]]; then
  echo "usage: $0 [seconds|one-hour|day|three-days] [all|gateway|agents|channels|config|security|secrets|clashd|install|host-agent]" >&2
  exit 2
fi

case "$surface" in
  all|gateway|agents|channels|config|security|secrets|clashd|install|host-agent) ;;
  *)
    echo "usage: $0 [seconds|one-hour|day|three-days] [all|gateway|agents|channels|config|security|secrets|clashd|install|host-agent]" >&2
    exit 2
    ;;
esac

start="$(date +%s)"
deadline="$((start + duration))"
artifact_dir="${BOUNDARY_EXPLORE_ARTIFACT_DIR:-boundary-artifacts/long-$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p "$artifact_dir"

property_cases="${PROPTEST_CASES_PER_RUN:-4096}"
fuzz_runs="${FUZZ_RUNS_PER_TARGET:-4096}"
iteration=0

remaining() {
  local now
  now="$(date +%s)"
  echo "$((deadline - now))"
}

still_running() {
  [[ "$(remaining)" -gt 0 ]]
}

run_logged() {
  local name="$1"
  shift

  if ! still_running; then
    return 1
  fi

  local log_file="$artifact_dir/$(printf '%04d' "$iteration")-${name}.log"
  printf '\n[%s] running %s (%ss remaining)\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$name" "$(remaining)"
  printf '$ %q' "$@" > "$log_file"
  printf '\n\n' >> "$log_file"

  if "$@" >> "$log_file" 2>&1; then
    printf '[%s] ok %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$name"
  else
    printf '[%s] FAILED %s; log: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$name" "$log_file" >&2
    tail -n 80 "$log_file" >&2 || true
    exit 1
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

run_gateway() {
  run_logged gateway-helicone env PROPTEST_CASES="$property_cases" cargo test -p calciforge proxy::helicone_streaming::tests -- --nocapture || return 1
  run_logged gateway-routing env PROPTEST_CASES="$property_cases" cargo test -p calciforge proxy::routing::tests -- --nocapture || return 1
  run_logged gateway-auth env PROPTEST_CASES="$property_cases" cargo test -p calciforge proxy::auth::tests -- --nocapture || return 1
}

run_agents() {
  run_logged agent-openclaw-callback env PROPTEST_CASES="$property_cases" cargo test -p calciforge adapters::openclaw_channel::openclaw_channel_reply_tests -- --nocapture || return 1
  run_logged agent-adapters env PROPTEST_CASES="$property_cases" cargo test -p calciforge adapters:: -- --nocapture || return 1
}

run_channels() {
  run_logged channel-adapters env PROPTEST_CASES="$property_cases" cargo test -p calciforge channels:: -- --nocapture || return 1
}

run_config() {
  run_logged config-routing env PROPTEST_CASES="$property_cases" cargo test -p calciforge config:: -- --nocapture || return 1
}

run_security() {
  run_logged security-fuzz env FUZZ_RUNS="$fuzz_runs" FUZZ_TARGETS="security_substitution_bytes security_substitution_valid_refs" bash scripts/boundary-fuzz.sh smoke || return 1
}

run_secrets() {
  run_logged secrets-fuzz env FUZZ_RUNS="$fuzz_runs" FUZZ_TARGETS="secret_metadata_destinations" bash scripts/boundary-fuzz.sh smoke || return 1
}

run_clashd() {
  run_logged clashd-fuzz env FUZZ_RUNS="$fuzz_runs" FUZZ_TARGETS="clashd_domain_lists" bash scripts/boundary-fuzz.sh smoke || return 1
}

run_install() {
  ensure_uv_for_hegel
  run_logged install-hegel cargo test -p calciforge --features hegel install:: -- --nocapture || return 1
  run_logged doctor-boundary env PROPTEST_CASES="$property_cases" cargo test -p calciforge doctor:: -- --nocapture || return 1
}

run_host_agent() {
  run_logged host-agent env PROPTEST_CASES="$property_cases" cargo test -p host-agent -- --nocapture || return 1
}

printf 'boundary long exploration: duration=%ss surface=%s artifacts=%s\n' "$duration" "$surface" "$artifact_dir"
printf 'PROPTEST_CASES_PER_RUN=%s FUZZ_RUNS_PER_TARGET=%s\n' "$property_cases" "$fuzz_runs"

while still_running; do
  iteration="$((iteration + 1))"
  case "$surface" in
    all)
      run_gateway || break
      run_agents || break
      run_channels || break
      run_config || break
      run_security || break
      run_secrets || break
      run_clashd || break
      run_install || break
      run_host_agent || break
      ;;
    gateway) run_gateway || break ;;
    agents) run_agents || break ;;
    channels) run_channels || break ;;
    config) run_config || break ;;
    security) run_security || break ;;
    secrets) run_secrets || break ;;
    clashd) run_clashd || break ;;
    install) run_install || break ;;
    host-agent) run_host_agent || break ;;
  esac
done

printf '\ncompleted boundary long exploration after %ss; artifacts: %s\n' "$(( $(date +%s) - start ))" "$artifact_dir"
