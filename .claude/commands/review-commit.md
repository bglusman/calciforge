---
name: review-commit
description: Run an adversarial review of a commit (or small range) via the commit-reviewer subagent. Findings return here for judgment; nothing is auto-applied.
argument-hint: "[ref] — default HEAD; accepts any git rev or range like HEAD~3..HEAD"
---

Arguments: `$ARGUMENTS` (may be empty → use `HEAD`).

When invoked:

1. Run `git show --stat <ref>` first to get a quick sense of scope.
   If the scope spans more than ~20 files or ~800 lines, warn that
   findings may be shallow and recommend breaking into smaller ranges.
2. Spawn the `commit-reviewer` subagent (subagent_type:
   `commit-reviewer`) with:
   - The full `git show <ref>` (or `git diff <base>..<head>` for a
     range) as the primary input.
   - Pointers to `docs/rfcs/agent-secret-gateway.md`,
     `docs/rfcs/consolidation-findings.md`, and `CLAUDE.md` so the
     reviewer knows the project invariants.
3. Report the subagent's findings verbatim — do not re-summarize or
   rewrite them. The reviewer's output is already terse by design.
4. For each **BLOCK** finding, propose a concrete fix (file:line +
   one-paragraph description) and ASK whether to apply it. Do not
   auto-apply anything from a subagent review.
5. For each **INCONSISTENCY** finding, same — propose, ask.
6. For each **TEST-QUALITY** finding, decide whether it warrants a
   follow-up test improvement PR or an immediate fixup. Note which
   way you're leaning and ask for confirmation.

The reviewer is deliberately terse and may return just "No blocking
findings." — that's a valid and common output; don't treat it as
underperformance. If every recent commit is clean, either you're
getting better or the reviewer prompt needs sharpening (surface
both as a data point over a handful of commits).

Do NOT use this command to review your OWN in-progress edits — only
commits that are already made. For unstaged work, review it yourself
inline; the subagent adds latency without value on code that hasn't
settled.
