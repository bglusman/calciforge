#!/usr/bin/env bash
# Build a release tarball layout suitable for Homebrew binary formulas and
# manual installs.
#
# Usage:
#   scripts/build-dist-archive.sh [version] [rust-target]
#
# Output:
#   dist/calciforge-<version>-<target>.tar.gz
#   dist/calciforge-<version>-<target>.tar.gz.sha256

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="${1:-}"
TARGET="${2:-}"

if [[ -z "$VERSION" ]]; then
    VERSION="$(
        cd "$ROOT"
        cargo metadata --no-deps --format-version 1 \
            | python3 -c 'import json,sys; data=json.load(sys.stdin); print(next(p["version"] for p in data["packages"] if p["name"] == "calciforge"))'
    )"
fi

if [[ -z "$TARGET" ]]; then
    TARGET="$(rustc -Vv | awk '/^host:/ {print $2}')"
fi

PROFILE="${CALCIFORGE_DIST_PROFILE:-dist}"
FNOX_VERSION="${FNOX_VERSION:-v1.23.1}"
DIST_DIR="$ROOT/dist"
STAGE="$DIST_DIR/calciforge-$VERSION-$TARGET"
ARCHIVE="$DIST_DIR/calciforge-$VERSION-$TARGET.tar.gz"
BIN_DIR="$ROOT/target/$TARGET/$PROFILE"

fnox_release_asset_and_sha_for_target() {
    case "$1" in
        x86_64-unknown-linux-gnu) echo "fnox-x86_64-unknown-linux-gnu.tar.gz 021c4fe683e109b616a4b936117c60e63f351b4000bd0999f0110f3088fc7f58" ;;
        aarch64-unknown-linux-gnu) echo "fnox-aarch64-unknown-linux-gnu.tar.gz de4c06fc8851ad4be8109d077d0362ee8603ae0aac0cbffb715ac89e2238f2c4" ;;
        x86_64-apple-darwin) echo "fnox-x86_64-apple-darwin.tar.gz 0b09405d648387a163d3f21be3e88972332d3fa04e5db55955726890a3f22877" ;;
        aarch64-apple-darwin) echo "fnox-aarch64-apple-darwin.tar.gz f92d60ee4bb669b97a500f000fe053f92f7cbf0817ed4678e5b0f506d1357dd6" ;;
        *) return 1 ;;
    esac
}

verify_sha256() {
    local expected="$1" path="$2"
    if command -v shasum >/dev/null 2>&1; then
        printf '%s  %s\n' "$expected" "$path" | shasum -a 256 -c -
    else
        printf '%s  %s\n' "$expected" "$path" | sha256sum -c -
    fi
}

install_fnox_companion() {
    local asset sha tmp
    read -r asset sha < <(fnox_release_asset_and_sha_for_target "$TARGET") || {
        echo "No fnox release asset mapping for target $TARGET" >&2
        return 1
    }
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"; trap - RETURN' RETURN
    curl -fsSL "https://github.com/jdx/fnox/releases/download/${FNOX_VERSION}/${asset}" \
        -o "$tmp/fnox.tar.gz"
    verify_sha256 "$sha" "$tmp/fnox.tar.gz"
    tar -xzf "$tmp/fnox.tar.gz" -C "$tmp"
    install -m 755 "$tmp/fnox" "$STAGE/bin/fnox"
}

cd "$ROOT"

echo "Building Calciforge $VERSION for $TARGET with profile '$PROFILE'"
cargo build --profile "$PROFILE" --target "$TARGET" \
    -p clashd \
    -p security-proxy \
    -p mcp-server \
    -p paste-server
cargo build --profile "$PROFILE" --target "$TARGET" \
    -p secrets-client --bin calciforge-secrets
cargo build --profile "$PROFILE" --target "$TARGET" \
    -p calciforge --features channel-matrix

rm -rf "$STAGE"
mkdir -p "$STAGE/bin"

for bin in calciforge clashd security-proxy mcp-server paste-server calciforge-secrets; do
    install -m 755 "$BIN_DIR/$bin" "$STAGE/bin/$bin"
done
install_fnox_companion

install -m 644 "$ROOT/LICENSE" "$STAGE/LICENSE"
install -m 644 "$ROOT/THIRD_PARTY_NOTICES.txt" "$STAGE/THIRD_PARTY_NOTICES.txt"
printf '%s\n' \
    "Calciforge $VERSION ($TARGET)" \
    "" \
    "Binaries are under ./bin. Manual archives include the fnox companion binary" \
    "used by Calciforge secret helpers. The Homebrew formula generated from" \
    "packaging/homebrew/calciforge.rb.in uses Homebrew's fnox dependency instead." \
    > "$STAGE/README.txt"

rm -f "$ARCHIVE" "$ARCHIVE.sha256"
tar -C "$DIST_DIR" -czf "$ARCHIVE" "calciforge-$VERSION-$TARGET"
if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$ARCHIVE" | awk '{print $1}' > "$ARCHIVE.sha256"
else
    sha256sum "$ARCHIVE" | awk '{print $1}' > "$ARCHIVE.sha256"
fi

echo "Wrote $ARCHIVE"
echo "SHA256: $(cat "$ARCHIVE.sha256")"
