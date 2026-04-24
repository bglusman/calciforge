#!/usr/bin/env bash
# install-git-hooks.sh — idempotent installer for the repo's git hooks.
# Safe to run multiple times. Won't clobber any hook already in place.
#
# Hooks installed:
#   .git/hooks/pre-commit → scripts/pre-commit-hook.sh  (fmt, clippy, gitleaks)
#   .git/hooks/pre-push   → scripts/pre-push.sh         (full tests + loom)

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

install_hook() {
    local name="$1"
    local source="$2"
    local target=".git/hooks/$name"
    local rel_source="../../$source"

    if [[ ! -f "$source" ]]; then
        echo "  ✗ source missing: $source"
        return 1
    fi

    if [[ -e "$target" ]] && [[ ! -L "$target" ]]; then
        echo "  ! $target exists and is not a symlink — leaving alone"
        return 0
    fi

    if [[ -L "$target" ]]; then
        local current="$(readlink "$target")"
        if [[ "$current" == "$rel_source" ]]; then
            echo "  = $name already linked"
            return 0
        fi
        echo "  ! replacing existing symlink ($current → $rel_source)"
        rm "$target"
    fi

    ln -s "$rel_source" "$target"
    chmod +x "$source"
    echo "  ✓ $name installed"
}

echo "Installing git hooks..."
install_hook pre-commit scripts/pre-commit-hook.sh
install_hook pre-push   scripts/pre-push.sh
echo "Done."
