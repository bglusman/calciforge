---
layout: default
title: Agent Handoff Checklist
status: Implemented
---

# Agent Handoff Checklist

Status: **Implemented** — this checklist is the expected maintainer handoff
format for agent-authored Calciforge changes.

Use this before asking another agent or human reviewer to continue a branch.
The goal is to preserve enough state to resume safely without re-reading the
whole transcript.

## Required handoff

- **Branch / PR:** branch name, PR number or URL, and base branch.
- **Intent:** one or two sentences describing the user-facing or operator-facing
  goal.
- **Current state:** what was changed, committed, pushed, or intentionally left
  uncommitted.
- **Verification:** exact local commands run and the relevant CI status.
- **Adversarial review:** commit SHA reviewed, who/what reviewed it, and any
  findings or explicit "no material findings" result.
- **Open risks:** known flakes, blocked checks, TODOs, or assumptions.
- **Next action:** the single next step a resumed agent should take.

## Commit discipline

After every commit, immediately run an adversarial review of that commit before
moving on. The review must be independent of CI and should look for security,
correctness, contract, test-fragility, and documentation issues beyond red
checks.

## Example

```text
Branch / PR: codex-doc-status-labels-and-handoff, dependent on PR #145
Intent: add public doc status labels and a maintainer handoff checklist.
Current state: docs edited locally, not pushed.
Verification: markdown lint/docs build pending.
Adversarial review: pending for next commit.
Open risks: branch is based on PR #145 until it merges.
Next action: run docs checks, commit, trigger adversarial review.
```
