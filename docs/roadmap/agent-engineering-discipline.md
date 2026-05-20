---
layout: default
title: AI-Assisted Engineering Discipline
---

# AI-Assisted Engineering Discipline

**Status:** Design sketch

Calciforge is itself a safety tool for AI agents, so the project has to be
honest about how agent-written code fails. The lesson from recent Calciforge
bugs, and from broader Rust-with-agents reports such as
[Cheng Huang's write-up](https://zfhuang99.github.io/rust/claude%20code/codex/contracts/spec-driven%20development/2025/12/01/rust-with-ai.html)
and the [Hacker News discussion](https://news.ycombinator.com/item?id=48205415),
is not "generate more code." It is: keep contracts explicit, make feedback
loops mechanical, and treat every generated test as suspicious until it proves
which promise it protects.

This page is maintainer-facing. It records how Calciforge agents and humans
should shape future work.

## Useful Lessons to Import

### Contracts before code

Before changing a boundary, write the contract in plain language:

- the preconditions Calciforge expects,
- the postconditions it promises,
- the invariants that must survive failure,
- the operator-visible behavior that proves the contract held.

This is especially important for model/provider adapters, security-proxy
rewrites, channel routing, secret handling, installer paths, and doctor checks.
Those are not "just implementation" areas. They are the castle doors.

Contracts do not need a formal language before they help. A short table in a
test, ADR, or roadmap note is enough if it names the failure mode clearly.

### One story per branch

A single user story is the right default unit for agent implementation. A story
can still cross several files, but it should have one visible outcome:

- "A first-class agent using `stream=true` gets a valid response through the
  configured provider."
- "`doctor` catches an ACP binary path that would fail at runtime."
- "A secret entered through the paste UI appears in `!secret list` with its
  destination policy."

When a branch starts solving three stories, split it. Calciforge already has
enough moving parts; one PR should not become a second moving castle.

### Feedback loops must be executable

Rust helps because the compiler, formatter, linter, and tests can push back on
agent mistakes. Use that. Every meaningful PR should name the narrowest checks
that exercise the contract it changes.

For Calciforge, the usual progression is:

1. reproduce the failure or write the contract test,
2. make the smallest behavioral change,
3. run focused tests for the touched boundary,
4. run the relevant docs/ratchet/doctor checks,
5. commit,
6. perform adversarial review on the diff.

Generated code is allowed. Unreviewed generated behavior is not.

### Test quality over test count

HN commenters pushed on the right weak spot: a large test count does not prove
much if nobody can say what those tests protect. Calciforge should judge tests
by contract value:

- Would this test fail on the bug we are fixing?
- Does it exercise behavior an operator or agent can observe?
- Does it cover legal variation from a real upstream, not only our ideal mock?
- If the implementation changed, would the test still describe the same
  promise?

The [failure discovery action plan](failure-discovery-action-plan.html) calls
these aggression tests when they deliberately search for likely future
failures. That should become normal for security, gateway, channel, installer,
and agent-adapter boundaries.

### Human-readable abstractions matter

One HN theme was that agents can make code grow faster than it becomes
understandable. Calciforge should resist that. A useful abstraction should make
the product contract easier to see:

- one place for model/provider selector resolution,
- one place for first-class adapter lifecycle metadata,
- one place for security proxy policy decisions,
- one place for channel command state,
- one place for installer ownership of each generated file or service.

If a change adds another "almost the same" path, treat that as architecture
debt even when tests pass.

### Measure before tuning

The article's performance loop is worth copying: instrument, run, analyze,
change one thing, and measure again. For Calciforge this applies to:

- model cold starts and local model swapping,
- provider retry behavior,
- gateway streaming latency,
- lock contention around channel/session state,
- doctor and install runtime,
- security-proxy scan overhead.

Do not merge speculative latency fixes that lack a baseline and a
post-change measurement. We have enough guesses; keep the ones with numbers.

## Required PR Checklist for Agent-Authored Changes

Use this checklist for branches substantially authored by an AI coding agent:

- **Story:** the PR names one user-visible story or boundary contract.
- **Contract:** changed boundaries name preconditions, postconditions, or
  invariants in tests, docs, or comments.
- **Regression:** bug fixes start with a failing test, or the PR explains why a
  reproducer is not practical and adds the closest executable guardrail.
- **Aggression:** new adapters, providers, channel paths, or installer surfaces
  add or update a high-risk scenario or boundary registry entry when the change
  creates a new failure shape.
- **Review:** after commit, the author performs adversarial review focused on
  security, architecture drift, code quality, and whether tests can really fail.
- **Measurement:** performance changes include before/after numbers and the
  command or script used to collect them.
- **No line-count trophy:** code volume is not presented as evidence of
  progress. Smaller, clearer patches win.

## Where This Should Become Automation

Near-term automation should make the good path easier:

- Extend PR templates to ask for story, contract, regression, and measurement
  sections.
- Teach `scripts/check-scenarios.py` to flag new adapter/provider/channel
  files that lack a high-risk scenario or boundary registry entry.
- Add a lightweight `scripts/check-agent-pr-discipline.py` if PR drift
  continues: changed boundary files should either link a scenario, add a test,
  or mark the exception explicitly.
- Add `doctor --live` checks for first-class agents so the same path users test
  manually is exercised by automation.
- Keep ratchets pointed at drift that matters: duplicated decision points,
  oversized responsibility modules, unowned background tasks, and stringly
  security decisions.

## Anti-Patterns

Avoid these failure modes:

- **Mock-shaped confidence:** tests that only prove Calciforge can parse the
  response shape it wished the upstream returned.
- **Generated-test fog:** many tests with unclear oracles, snapshots, or
  implementation-detail assertions.
- **Architecture by accumulation:** adding a second source of truth because it
  is faster than routing through the existing one.
- **Tool-result obedience:** accepting compiler or linter suggestions without
  checking whether they address the root cause.
- **Performance folklore:** changing async, locks, cloning, retries, or local
  model settings because they "seem slow" without measurements.

The bar is simple: every agent-aided change should leave the contract clearer
than it found it. If the change only leaves more code, Calcifer is allowed to
complain.
