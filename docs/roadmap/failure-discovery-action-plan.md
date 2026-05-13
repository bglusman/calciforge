---
layout: default
title: Failure Discovery Action Plan
---

# Failure Discovery Action Plan

**Status:** Design sketch

Calciforge already has a lot of tests. Recent staging bugs show that count is
not the same thing as confidence. The recurring problem is narrower: tests often
cover one observed shape, while production breaks on another valid shape emitted
by a real agent, gateway, package manager, filesystem, service manager, or
channel.

This page records the lesson and turns it into work. The aim is not only more
regression tests. Calciforge needs aggression tests: checks that deliberately
break protocols, runtime state, config paths, and trust assumptions in ways we
can already foresee.

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

## Aggression Testing

### Boundary-contract rule

Every surface that accepts data, bytes, process output, filesystem state, time,
or network behavior Calciforge does not control must declare two contracts:

- **Invalid-input containment:** arbitrary out-of-contract input may be
  rejected, ignored, quarantined, retried, or classified as degraded, but it
  must not panic, wedge a worker, leak secrets, corrupt durable state, bypass
  authorization, exhaust unbounded resources, or misclassify the failure as an
  unrelated subsystem.
- **Valid-input correctness:** any input valid under the external contract must
  be handled correctly across legal shape variation: field order, optional
  fields, extra fields, chunk boundaries, line endings, retries, duplicate
  delivery, latency, process output timing, filesystem permissions, and
  platform packaging differences.

This is deliberately not a test of a single client's behavior. The unit under
test is the boundary contract. A test that only proves one mocked client
response still works is not enough for a boundary.

### Bounded and unbounded behavior

For each boundary, classify behavior before writing tests:

- **Bounded invalid:** malformed but size-limited examples we can enumerate:
  missing required JSON fields, bad auth tokens, invalid TOML, unknown sender
  aliases, invalid HMACs, unsupported content types, nonzero subprocess exit,
  or a config value outside the accepted domain.
- **Unbounded invalid:** arbitrary bytes or environment behavior: invalid UTF-8,
  huge bodies, broken pipes, truncated streams, duplicate headers, symlinks,
  slow partial IO, retry storms, random process stdout/stderr, corrupt SQLite
  files, and hostile web content.
- **Bounded valid:** explicitly supported contract variants:
  OpenAI-compatible JSON and SSE, Matrix and Telegram update shapes,
  Signal/WhatsApp normalized messages, configured model selectors, approved
  filesystem layouts, and known service-manager templates.
- **Unbounded valid:** legal variation inside those contracts: chunking, CRLF
  vs LF, extra JSON fields, unknown-but-preserved request extensions, event
  ordering allowed by the upstream protocol, provider-specific error envelopes,
  repeated callbacks, and platform path differences.

The first category gets table-driven regressions. The second gets fuzzing and
chaos. The third gets golden fixtures and structured generators. The fourth
gets property tests, differential checks, and integration simulators.

### Two required test tiers

Every boundary must eventually have both tiers:

1. **Invalid-data containment tests.** Feed malformed or adversarial inputs and
   assert the system remains bounded: no panic, no hang, no secret exposure, no
   durable corruption, no authorization bypass, clear failure classification,
   and bounded resource use.
2. **Valid-data correctness tests.** Generate or replay valid inputs with many
   legal shapes and assert the normalized user-visible result is correct. These
   tests should use protocol-level or domain-level oracles, not private helper
   state.

Property tests such as Hegel/proptest are one layer. They do not replace raw
byte fuzzing, real fixture corpora, mock upstream servers, subprocess
simulators, filesystem chaos, or deterministic and fault-injected integration
tests. Use Hegel where the input space is typed and the invariant is crisp; use
`cargo-fuzz`/`arbitrary` where bytes cross a parser boundary; use simulators
where ordering, timing, retries, and partial failure matter.

### Integration boundary inventory

| Surface | Bounded behavior | Unbounded behavior | Tier 1 invalid containment | Tier 2 valid correctness |
| --- | --- | --- | --- | --- |
| OpenAI-compatible model gateway and provider adapters | Request schema, auth headers, model selectors, content type, provider status class | SSE frame boundaries, extra fields, provider-specific error bodies, huge/non-UTF8 bodies, transport timing | Fuzz JSON/SSE parsers, provider error envelopes, headers, timeout/read errors | Generate valid JSON/SSE/tool-call streams, replay provider corpora, assert normalized `ChatCompletionResponse` and failure classification |
| Agent adapters: OpenClaw, ZeroClaw, IronClaw, Hermes, OpenAI-compatible HTTP | Callback tokens, request IDs, session keys, attachment limits, response status | Duplicate/stale callbacks, partial responses, callback races, malformed attachments, HTML error pages | Generate malformed callbacks and agent responses; assert reject/drop without waking wrong request | Simulate valid callbacks and responses across ordering/latency variants; assert one reply maps to the correct pending request |
| Process adapters: CLI, artifact CLI, Codex/Claude/Kimi/Dirac, ACP/ACPX | Command path, argv/env templates, stdin protocol, timeout budget, artifact root | Non-UTF8 stdout/stderr, broken stdin, hung child, partial JSON-RPC frames, symlink artifact trees, huge outputs | PTY/subprocess simulator with random stdout/stderr/exit/timing; artifact filesystem fuzz | Valid protocol frame generators and golden CLI transcript corpora; assert normalized agent output/artifacts |
| Channel adapters: Telegram, Matrix, Signal, WhatsApp, SMS/Linq, mock | Sender identity, auth/HMAC, event type, message text, group/reply target, timestamp | Duplicate/out-of-order events, missing reply target, non-text attachments, confusable IDs, provider rate-limit bodies | Generate malformed webhooks/updates/events and normalized `ChannelMessage` variants; assert unauthorized or unusable input drops safely | Golden corpora and structured generators for valid channel events; assert correct identity, routing, reply target, command state, and outbound response |
| Config, identity, auth, routing, model selectors | TOML/JSON schema, unique IDs, alias format, route graph, allowlists, context-window declarations | Duplicate/cyclic aliases, numeric overflows, unknown legacy fields, unsafe globs, huge configs | Fuzz TOML/JSON loaders and validators; assert typed errors and no partial unsafe config | Generate valid graph variations; assert same auth/routing decisions across aliases, shortcuts, roles, alloys, cascades, and dispatchers |
| Security proxy, adversary detector, scanner policy | URL policy, secret reference grammar, scanner result schema, upstream method/header/body contracts | Header smuggling, compressed/chunked/binary bodies, redirects, hostile HTML/JSON, malformed Starlark returns | Fuzz URL/header/body substitution, response scanning, policy loaders; assert block/allow decisions fail closed where required | Replay valid browser/provider traffic through mock upstreams; assert secrets substitute only for allowed destinations and blocked content never reaches model context |
| Secrets: fnox, MCP server, paste server, metadata, `.env` ingestion | Secret names, destination metadata, token expiry, Origin/Referer policy, MCP params | Missing/hung/non-UTF8 fnox, replayed paste tokens, malformed `.env`, racey submissions | Fake fnox process and HTTP fuzz for paste/MCP; assert no secret leak and correct auth failure | Generate valid secret refs, metadata, and `.env` entries; assert correct vault operation and destination policy |
| Filesystem and persistence | Config paths, artifact roots, context DB schema, cache/log locations, package layout | Symlinks, permission errors, missing dirs, corrupt SQLite, concurrent writes, stale service files | Filesystem simulator/tempdir chaos; corrupt DB/config files; assert no data loss outside owned paths | Valid path/layout generators for source/Homebrew/Docker/user/system installs; assert doctor/runtime use the same files |
| Installer, doctor, service managers, local model lifecycle, hooks | OS/service template schema, SSH target, remote config path, hook env vars, switch timeout | Stale launchd/systemd units, missing binaries, SSH partial failures, hook hangs, concurrent switch storms | Mock SSH/service/process faults; assert rollback or explicit degraded result | Generate valid install/update layouts and hook envs; assert doctor checks the same path the user action will use |
| Host-agent RPC, approval, ZFS/git/pct/systemd adapters | mTLS identity, operation schema, approval token TTL, dataset/snapshot naming | Spoofed webhook, replayed approval, path/shell metacharacters, RPC JSON drift, adapter rate-limit variants | Generate malformed RPC/approval payloads; assert auth rejection and no host mutation | Valid operation sequence generators and mock host adapters; assert approved operations mutate only intended resources |
| Clashd/domain lists/policy hooks | Hook payload schema, domain grammar, policy result shape, remote list format | Huge lists, redirects, punycode/wildcards, malformed hooks, policy recursion/timeout | Fuzz domain normalization and policy payloads; assert deny/fail-closed rules hold | Replay valid hook/domain-list corpora; assert deterministic policy decisions |
| Time, concurrency, retries, and background state | TTLs, retry budgets, progress cadence, request correlation, cancellation semantics | Clock jumps, duplicate delivery, retry storms, partial cancellation, stale numbered-choice state | Loom/deterministic simulation/fault injection for shared state and retry paths | Valid operation sequences with randomized timing; assert exactly-once or at-most-once semantics where promised |

### 1. Scenario catalog before broad test growth

Keep a small checked-in catalog of high-risk product scenarios. Each scenario
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

The first catalog lives at `tests/scenarios/high-risk-scenarios.json` and is
validated by `scripts/check-scenarios.py` in CI. It is intentionally not a
marketing roadmap. It is a list of assumptions we expect future tests and live
smokes to attack.

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

### 4. Property, fuzz, and simulation tests where boundaries make decisions

Use property tests for typed invariants:

- SSE parsing and chunk assembly,
- model/alias/provider selector resolution,
- secret placeholder recognition,
- per-secret destination matching,
- channel command parsing and numbered-choice state.

Use fuzzing where malformed bytes can cross a trust boundary:

- HTTP headers and URLs,
- JSON tool-call deltas,
- secret reference syntax,
- adversarial scanner payloads.

Use simulation or chaos where the boundary is temporal or stateful:

- subprocess stdout/stderr timing,
- partial network reads and retries,
- duplicate webhooks and callback races,
- filesystem permissions and symlinks,
- concurrent model switches and stale session state.

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

1. Add real-shape fixtures for streaming chat completions and tool calls.
2. Create a checked-in boundary corpus for provider, channel, agent callback,
   config, filesystem, and subprocess fixtures.
3. Add per-boundary two-tier tests: invalid containment first, valid generalized
   correctness second.
4. Extend provider-adapter tests to cover non-retryable failures and alias
   resolution through the same code used at runtime.
5. Add a `doctor --live` path for first-class agent smoke tests.
6. Run a tiny mutation pass on selector resolution and security-proxy policy.
7. Add a release-candidate checklist item: one manually observed failure must
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
