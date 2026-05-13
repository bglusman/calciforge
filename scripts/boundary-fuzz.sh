#!/usr/bin/env bash
set -euo pipefail

mode="${1:-smoke}"

case "$mode" in
  smoke)
    runs="${FUZZ_RUNS:-512}"
    ;;
  nightly)
    runs="${FUZZ_RUNS:-8192}"
    ;;
  *)
    echo "usage: $0 [smoke|nightly]" >&2
    exit 2
    ;;
esac

if ! rustup toolchain list | grep -q '^nightly'; then
  rustup toolchain install nightly --profile minimal
fi

if ! cargo fuzz --version >/dev/null 2>&1; then
  cargo install cargo-fuzz --locked
fi

if [[ -n "${FUZZ_TARGETS:-}" ]]; then
  read -r -a targets <<< "$FUZZ_TARGETS"
else
  targets=(
    security_substitution_bytes
    security_substitution_valid_refs
    secret_metadata_destinations
    clashd_domain_lists
  )
fi

for target in "${targets[@]}"; do
  if [[ -n "${GITHUB_ACTIONS:-}" ]]; then
    echo "::group::cargo fuzz run ${target}"
  else
    printf '\n==> cargo fuzz run %s -runs=%s\n' "$target" "$runs"
  fi
  cargo +nightly fuzz run "$target" -- -runs="$runs"
  if [[ -n "${GITHUB_ACTIONS:-}" ]]; then
    echo "::endgroup::"
  fi
done
