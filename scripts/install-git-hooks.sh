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
        echo "  ✗ source missing: $source" >&2
        return 1
    fi

    # Pre-existing non-symlink hook: back it up (so nothing is lost)
    # and continue with the install. The user/reviewer may want the
    # old hook back; `.git/hooks/<name>.backup-<timestamp>` makes
    # that a one-step restore. Returning nonzero on this branch
    # would also be defensible, but the silent-no-op the original
    # code did was the real bug — the user believed the install
    # succeeded when their old hook was still the active one.
    if [[ -e "$target" ]] && [[ ! -L "$target" ]]; then
        local backup="$target.backup-$(date +%Y%m%dT%H%M%S)"
        mv "$target" "$backup"
        echo "  ! backed up existing $name → $backup"
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
