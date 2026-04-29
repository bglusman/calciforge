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
# Parse it properly so we only fire on actual `git commit` invocations
# — not `git commit --dry-run`, `git commit --help`, `echo "git commit"`,
# or any other Bash call that happens to contain the substring.
TOOL_INPUT="$(cat || true)"

# Extract the actual command from the JSON. Require jq — it's present
# on every platform we support via the installer's toolchain. If jq is
# missing we bail cleanly rather than falling back to a permissive
# grep that fires on text-about-commits.
if ! command -v jq >/dev/null 2>&1; then
    exit 0
fi
CMD="$(printf '%s' "$TOOL_INPUT" | jq -r '.tool_input.command // empty' 2>/dev/null || true)"
if [[ -z "$CMD" ]]; then
    exit 0
fi

# Normalize: strip leading `cd …&&` wrappers and any `env` prefix so
# `cd /path && git commit -m "..."` still matches.
NORMALIZED="$(printf '%s' "$CMD" | tr -s ' ')"

# Only act on commands that *run* git commit, not describe it.
# - Must contain `git commit` as a standalone action
# - Must NOT be a dry-run / help / man / discussion form
case " $NORMALIZED " in
    *" git commit --dry-run"*|*" git commit -n --dry-run"*|*" git commit --help"*|*" man git-commit "*)
        exit 0
        ;;
esac
if ! printf '%s' "$NORMALIZED" | grep -qE '(^|&&|;|\|\|)[[:space:]]*git commit([[:space:]]|$)'; then
    exit 0
fi

# Repo root — resolve through git so we don't depend on CWD.
REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)"
if [[ -z "$REPO_ROOT" ]]; then
    exit 0
fi

cd "$REPO_ROOT"

# Newest commit SHA. Compare to the previously-seen HEAD (stored in a
# tiny state file) so a Bash call that didn't actually produce a new
# commit can't queue a review. This is the backstop for the JSON-parse
# check above — belt and suspenders.
SHA="$(git rev-parse HEAD 2>/dev/null || true)"
[[ -n "$SHA" ]] || exit 0

LAST_SHA_FILE="$REPO_ROOT/.claude/.last-reviewed-sha"
if [[ -f "$LAST_SHA_FILE" ]] && [[ "$(cat "$LAST_SHA_FILE")" == "$SHA" ]]; then
    # HEAD hasn't moved since last invocation — no new commit.
    exit 0
fi

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

# Record the SHA we just queued so a subsequent hook invocation for
# the same commit (e.g. during retry or across tool calls) skips.
printf '%s' "$SHA" > "$LAST_SHA_FILE"

# A zero-byte trigger file so the next turn's context includes a
# reminder without needing to list the directory. Claude can grep
# for this to decide whether to run /review-commit.
touch "$REPO_ROOT/.claude/REVIEW_PENDING"

# Print a notification — Claude Code includes PostToolUse hook stdout
# in the context message shown after the tool call, so this appears
# immediately and prompts the review without manual intervention.
SUBJECT="$(git log -1 --format=%s "$SHA")"
printf '\n[post-commit hook] Commit queued for adversarial review:\n'
printf '  SHA:     %s\n' "$SHA"
printf '  Subject: %s\n' "$SUBJECT"
printf 'Run /review-commit to review this commit now.\n'

exit 0
