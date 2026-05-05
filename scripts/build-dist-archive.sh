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
DIST_DIR="$ROOT/dist"
STAGE="$DIST_DIR/calciforge-$VERSION-$TARGET"
ARCHIVE="$DIST_DIR/calciforge-$VERSION-$TARGET.tar.gz"
BIN_DIR="$ROOT/target/$TARGET/$PROFILE"

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

install -m 644 "$ROOT/LICENSE" "$STAGE/LICENSE"
printf '%s\n' \
    "Calciforge $VERSION ($TARGET)" \
    "" \
    "Binaries are under ./bin. Add that directory to PATH, or install them" \
    "through the Homebrew formula generated from packaging/homebrew/calciforge.rb.in." \
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
