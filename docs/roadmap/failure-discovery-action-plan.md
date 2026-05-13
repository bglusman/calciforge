---
layout: default
title: Failure Discovery Action Plan
---

# Failure Discovery Action Plan

Calciforge already has a lot of tests. Recent staging bugs show that count is
not the same thing as confidence. The recurring problem is narrower: tests often
cover the shape we expected, while production breaks on the shape a real agent,
gateway, package manager, or service manager actually emits.

This page records the lesson and turns it into work.

## Post-mortem: Helicone Streaming Response Failure

### What happened

OpenClaw sent a normal chat-completions request through Calciforge with
`stream=true` and tools enabled. Helicone returned a valid
`text/event-stream` response. Calciforge treated the upstream body as a single
JSON chat-completion object and failed to decode it.

From the user's view, the local OpenClaw agent timed out, retried, and then
looked wedged. The services were mostly alive; the contract between two live
components was wrong.

### Why the tests missed it

- The adapter tests covered JSON responses, but not the common SSE response
  format used when `stream=true`.
- The mocked gateway behaved like our expectation, not like the real gateway.
- Smoke tests checked service availability and simple model calls, but not the
  exact request shape used by first-class agents.
- The failure crossed boundaries: OpenClaw request shaping, Calciforge adapter
  parsing, Helicone's response protocol, and local model latency all overlapped.

### Fix

Calciforge now accepts upstream `text/event-stream` chat-completion responses
from Helicone and folds them into the existing internal `ChatCompletionResponse`
type. Regression tests cover streamed content and streamed tool-call argument
chunks while preserving `stream=true` in the outbound request.

This is a compatibility fix, not the final streaming design. Today the adapter
still aggregates the upstream stream before Calciforge emits its response. True
token-through streaming needs a wider gateway trait and handler change.

## Pattern: The Bugs We Keep Finding Late

Recent failures tend to fall into a few buckets:

- **Protocol shape drift:** real services return SSE, tool-call chunks,
  alternate error envelopes, or partial metadata that mocks did not model.
- **Boundary mismatch:** traffic that should go through the gateway, proxy, or
  doctor path silently goes around it.
- **Config identity confusion:** model, alias, synthetic route, provider, and
  agent names can look interchangeable until one path treats them differently.
- **Runtime packaging drift:** Homebrew, Docker Compose, systemd, launchd, and
  manually repaired installs can run different binaries or configs.
- **Stale-session behavior:** a healthy route becomes unusable because an agent
  carries too much context, retries oddly, or holds on to broken state.
- **Weak doctor coverage:** `doctor` can pass while the next real user action
  fails because the check did not exercise the same path.

## Better Failure Discovery

### 1. Scenario catalog before broad test growth

Create a small checked-in catalog of high-risk product scenarios. Each scenario
must name:

- the user action,
- the components crossed,
- the security or reliability promise at stake,
- the exact observable failure that would matter.

Examples:

- "First-class agent sends `stream=true` with tool calls through the configured
  model provider."
- "Agent fetches a hostile web page through the security boundary and receives
  filtered content."
- "A package-installed Calciforge instance starts with the same config path that
  `doctor` validates."

Every new adapter, provider, channel, or installer path should add or update at
least one scenario.

### 2. Contract tests at every external boundary

For each provider adapter and first-class agent adapter, keep tests that use
wire-level fixtures from real services:

- OpenAI-compatible JSON success and error bodies.
- SSE chat-completion streams.
- Streamed tool-call chunks split across frames.
- Retryable and non-retryable errors.
- Auth failures and model-not-found failures.

Mocks should imitate real captures, not idealized structs.

### 3. Differential smoke tests

For release candidates, send the same small prompt through:

- direct configured model provider,
- Calciforge model route,
- one first-class agent route,
- one channel route.

The assertion is not that latency or wording matches. The assertion is that
failures classify correctly: provider failure, Calciforge failure, agent
failure, or channel failure.

### 4. Property and fuzz tests where parsers make decisions

Use property tests for:

- SSE parsing and chunk assembly,
- model/alias/provider selector resolution,
- secret placeholder recognition,
- per-secret destination matching,
- channel command parsing and numbered-choice state.

Use fuzzing where malformed input can cross a trust boundary:

- HTTP headers and URLs,
- JSON tool-call deltas,
- secret reference syntax,
- adversarial scanner payloads.

These tests should assert invariants, not snapshots. Examples:

- malformed chunks never panic,
- unknown placeholders never substitute,
- denied destinations never become allowed after URL normalization,
- an expired numbered-choice prompt cannot trigger later by accident.

### 5. Mutation tests on the small set of critical modules

Run mutation tests selectively. Whole-workspace mutation testing is too slow and
too noisy right now.

Start with:

- model/provider selector resolution,
- security-proxy substitution and destination policy,
- Helicone/LiteLLM provider adapters,
- first-class agent gateway enforcement,
- command-state expiry.

If a mutation survives in one of these modules, either improve the test or
decide that the branch is dead code and remove it.

### 6. Doctor must test the path users actually take

`calciforge doctor` should not stop at "port is open" or "config parses." For
first-class support, it should execute the same high-level path the user will
use:

- provider route accepts the configured model name or alias,
- first-class agent can make one bounded model call through Calciforge,
- channel can send a synthetic command and receive a response,
- security proxy can block a known canary response,
- secret list/input/use paths share the same vault and metadata.

When a check would be expensive, doctor should mark it as a skipped live check
with the exact command needed to run it.

### 7. Read the upstream docs when behavior depends on them

Before shipping an adapter or changing a protocol path, capture the upstream
contract in the PR:

- streaming response shape,
- retry behavior,
- API-key ownership model,
- session lifecycle,
- health-check endpoint,
- known unsupported fields.

This is not a paperwork exercise. It is how we avoid learning basic protocol
facts from a user's failed test message.

## Near-term Work

1. Add a scenario catalog under `docs/staging-test-matrix.md` or
   `tests/scenarios/`.
2. Add real-shape fixtures for streaming chat completions and tool calls.
3. Extend provider-adapter tests to cover non-retryable failures and alias
   resolution through the same code used at runtime.
4. Add a `doctor --live` path for first-class agent smoke tests.
5. Run a tiny mutation pass on selector resolution and security-proxy policy.
6. Add a release-candidate checklist item: one manually observed failure must
   become either an automated regression test or a documented impossible-to-test
   gap before the PR merges.

## Test Quality Standard

A useful regression test should answer three questions:

- Would it have failed before the fix?
- Does it exercise the user-visible contract, not only an implementation detail?
- Would it fail if the same bug came back through a different adapter or package
  path?

If the answer to the first question is "no," the test may still be useful, but
it is not a regression test. Label it honestly.
