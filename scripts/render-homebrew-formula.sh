#!/usr/bin/env bash
# Render packaging/homebrew/calciforge.rb.in with release URLs and checksums.
#
# The template is intentionally not a live Formula until a release has
# platform tarballs. This script creates the file that should be copied into a
# tap after CI or a release operator has produced archives with
# scripts/build-dist-archive.sh.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TEMPLATE="$ROOT/packaging/homebrew/calciforge.rb.in"
OUTPUT="${CALCIFORGE_FORMULA_OUTPUT:-$ROOT/dist/homebrew/calciforge.rb}"

usage() {
    cat <<'USAGE' >&2
Usage:
  scripts/render-homebrew-formula.sh --version <version> \
    --mac-arm64-sha256 <sha> --mac-intel-sha256 <sha> --linux-amd64-sha256 <sha> \
    [--base-url <url>] [--output <path>]

Default base URL:
  https://github.com/bglusman/calciforge/releases/download/v<version>
USAGE
}

VERSION=""
BASE_URL=""
MAC_ARM64_SHA256=""
MAC_INTEL_SHA256=""
LINUX_AMD64_SHA256=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version) VERSION="${2:-}"; shift 2 ;;
        --base-url) BASE_URL="${2:-}"; shift 2 ;;
        --mac-arm64-sha256) MAC_ARM64_SHA256="${2:-}"; shift 2 ;;
        --mac-intel-sha256) MAC_INTEL_SHA256="${2:-}"; shift 2 ;;
        --linux-amd64-sha256) LINUX_AMD64_SHA256="${2:-}"; shift 2 ;;
        --output) OUTPUT="${2:-}"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
    esac
done

[[ -n "$VERSION" && -n "$MAC_ARM64_SHA256" && -n "$MAC_INTEL_SHA256" && -n "$LINUX_AMD64_SHA256" ]] || {
    usage
    exit 2
}

if [[ -z "$BASE_URL" ]]; then
    BASE_URL="https://github.com/bglusman/calciforge/releases/download/v$VERSION"
fi

mkdir -p "$(dirname "$OUTPUT")"

MAC_ARM64_URL="$BASE_URL/calciforge-$VERSION-aarch64-apple-darwin.tar.gz"
MAC_INTEL_URL="$BASE_URL/calciforge-$VERSION-x86_64-apple-darwin.tar.gz"
LINUX_AMD64_URL="$BASE_URL/calciforge-$VERSION-x86_64-unknown-linux-gnu.tar.gz"
export VERSION MAC_ARM64_URL MAC_ARM64_SHA256 MAC_INTEL_URL MAC_INTEL_SHA256 \
    LINUX_AMD64_URL LINUX_AMD64_SHA256

python3 - "$TEMPLATE" "$OUTPUT" <<'PY'
import os
import sys

template, output = sys.argv[1], sys.argv[2]
text = open(template, encoding="utf-8").read()
for key in [
    "VERSION",
    "MAC_ARM64_URL",
    "MAC_ARM64_SHA256",
    "MAC_INTEL_URL",
    "MAC_INTEL_SHA256",
    "LINUX_AMD64_URL",
    "LINUX_AMD64_SHA256",
]:
    text = text.replace(f"__{key}__", os.environ[key])
open(output, "w", encoding="utf-8").write(text)
PY

ruby -c "$OUTPUT" >/dev/null
echo "Wrote $OUTPUT"
