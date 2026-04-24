#!/usr/bin/env bash
# claude-post-commit.sh — fires after a `git commit` that Claude Code
# runs via the Bash tool. Writes the commit diff to a pending-review
# file so the next Claude turn sees a notification and runs
# `/review-commit`.
#
# Intentionally does NOT call the Claude API directly from here —
# nested/fresh sessions are unpredictable and cost is unknown. The
# cheap, reliable path is: record the diff, let the main session pick
# it up on the next turn and spawn the subagent via the Task tool.
#
# Invocation: wired via `.claude/settings.local.json` PostToolUse hook
# on the `Bash` matcher. The hook receives the bash invocation on
# stdin (JSON); we only act when the command was a `git commit`.

set -euo pipefail

# The Claude Code hook passes tool input/output as JSON on stdin.
# We grep defensively rather than parsing — jq may not be installed
# everywhere, and this script needs to be a no-op failure if the
# environment is unexpected.
TOOL_INPUT="$(cat || true)"

# Bail silently if this isn't a git-commit invocation. We look for the
# literal "git commit" substring in the tool input — a couple of false
# positives (e.g. `echo "git commit"` in an explanation) are fine
# because the rest of the script checks for an actual new commit.
if ! printf '%s' "$TOOL_INPUT" | grep -q 'git commit'; then
    exit 0
fi

# Repo root — resolve through git so we don't depend on CWD.
REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)"
if [[ -z "$REPO_ROOT" ]]; then
    exit 0
fi

cd "$REPO_ROOT"

# Newest commit SHA. If HEAD hasn't moved since the last file we
# wrote, the bash call didn't actually produce a commit (maybe it was
# `git commit --dry-run` or a noop). Bail.
SHA="$(git rev-parse HEAD 2>/dev/null || true)"
[[ -n "$SHA" ]] || exit 0

REVIEW_DIR="$REPO_ROOT/.claude/pending-reviews"
mkdir -p "$REVIEW_DIR"

OUT="$REVIEW_DIR/$SHA.md"
if [[ -f "$OUT" ]]; then
    # Already queued — nothing to do. (This can happen if the hook
    # fires twice for the same commit, e.g. retry logic.)
    exit 0
fi

# Write header + diff. Keep the diff bounded — if someone committed a
# 50 KB lockfile change, the reviewer doesn't need every byte. 400 KB
# is generous for a typical commit while staying fast to process.
{
    printf '# Pending review: %s\n\n' "$SHA"
    printf '- Subject: %s\n' "$(git log -1 --format=%s "$SHA")"
    printf '- Author:  %s\n' "$(git log -1 --format='%an <%ae>' "$SHA")"
    printf '- Date:    %s\n\n' "$(git log -1 --format=%ai "$SHA")"
    printf '## Diff\n\n```diff\n'
    git show --format='' "$SHA" | head -c 400000
    printf '\n```\n'
} > "$OUT"

# A zero-byte trigger file so the next turn's context includes a
# reminder without needing to list the directory. Claude can grep
# for this to decide whether to run /review-commit.
touch "$REPO_ROOT/.claude/REVIEW_PENDING"

exit 0
