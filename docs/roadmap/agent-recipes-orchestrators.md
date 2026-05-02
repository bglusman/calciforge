---
layout: default
title: Agent Recipes and Orchestrators
---

# Agent Recipes, Artifacts, and Orchestrators

Calciforge should support more agent runtimes without pretending every upstream
tool deserves first-class adapter maintenance. The working direction is a
three-part vocabulary:

- **Recipes** — documented, security-aware command configurations for local
  tools such as npcsh, opencode profiles, or one-off media agents.
- **Adapters** — first-class protocol integrations used when Calciforge needs
  upstream-specific parsing, callbacks, approval pauses, event streams, or
  native session state.
- **Orchestrators** — async work backends such as Gas Town, where Calciforge
  should submit work, receive a job receipt, monitor status, relay progress,
  and deliver final summaries/artifacts.

## Working Now

- `kind = "artifact-cli"` runs a command with a Calciforge-controlled per-run
  artifact directory.
- The user task is sent on stdin.
- `{artifact_dir}` and `CALCIFORGE_ARTIFACT_DIR` expose where generated files
  should be written.
- Calciforge validates that artifacts remain under the run directory and stay
  below the current size limit.
- Artifact CLI and OpenClaw callback artifacts use the same Calciforge-owned
  local storage helpers; new runs opportunistically clean up artifact run
  directories older than 24 hours.
- Telegram and Matrix use the internal outbound-message envelope and send
  supported artifacts through native media APIs. Channels without native media
  support render text fallback with attachment names and sizes.

## Near-Term Work

- Add recipe examples for npcsh, opencode/OmO profiles, and other local agent
  CLIs after smoke-testing installed versions.
- Use `examples/agent-recipes/` for synchronous artifact-producing recipes and
  `examples/orchestrator-recipes/` for submit/status/final-result patterns such
  as Gas Town and OmO.
- Promote first-class managed agents through one installer pattern: remote
  config patching, inbound auth, reply callbacks, policy/proxy configuration,
  health checks, and rollback notes. OpenClaw is the first concrete
  implementation; ZeroClaw and future first-class agents should use the same
  shape instead of bespoke setup instructions.
- Evaluate Zed's Apache-2.0 ACP work as the reference implementation path for
  richer coding-agent sessions. In particular, `codex-acp` already wraps Codex
  CLI behind ACP with images, tool-call permission requests, edit review, TODO
  lists, slash commands, MCP server forwarding, and Codex auth methods.
- Add channel capability reporting so channels can say whether they prefer
  native media upload or text links.
- Cut all chat channels over through the same outbound-message contract.
- Define an agent-accessible Calciforge API so first-class agents such as
  OpenClaw can request artifacts, progress updates, policy checks, and final
  delivery without learning each chat channel's mechanics.

## First-Class Policy Wiring

Calciforge's security promise should not depend on an agent runtime politely
following prompt instructions. First-class agents need a path to submit
structured action intent to Calciforge's policy layer before the action runs.
The policy should decide allow, review, or deny while preserving enough audit
detail to explain the decision later.

The current target shape is:

- **Native policy adapter where available** — install an agent-specific hook,
  plugin, or ACP/MCP tool bridge that calls Calciforge/Clash before tool
  execution.
- **Security proxy and model gateway for network/provider traffic** — keep the
  HTTP(S) path easy to use by default, but do not pretend it covers every local
  command or runtime tool.
- **Container, VM, or host egress controls for stronger bypass resistance** —
  use network-level controls when the operator needs enforcement beyond a
  cooperative proxy environment variable.
- **Recipe-level wrappers for best-effort tools** — when an upstream runtime
  lacks hooks, document the remaining gap and keep the integration out of the
  first-class support tier.

OpenClaw is the first managed-agent path that should receive the full pattern:
channel plugin, reply callbacks, native policy plugin, proxy configuration,
health checks, and rollback behavior. Claude Code has a hook-based path.
Codex should move toward ACP plus Calciforge-owned tools where possible.
opencode should prefer ACP or a maintained plugin path once the interface is
stable.

ZeroClaw needs an explicit product decision. The official release can remain a
supported or best-effort adapter, but Calciforge should not inherit static
upstream paranoia as the main policy contract if that blocks useful actions
that Calciforge policy would otherwise allow. If upstream does not expose a
blocking, configurable policy hook, a fork or library-mode integration may be
justified. Reviving a name such as NonZeroClaw could make sense for that path:
a ZeroClaw-compatible runtime whose policy authority is Calciforge/Clash, while
official ZeroClaw remains the stricter compatibility option.

Each first-class agent should eventually have a short support matrix:

- chat/session bridge,
- native policy hook status,
- proxy/model-gateway wiring,
- secret discovery path,
- artifact/callback support,
- install/upgrade ownership,
- known bypasses or stricter-than-policy limits.

## ACP Direction

ACP is the most promising common protocol layer for interactive coding agents.
Calciforge already has `kind = "acp"` and `kind = "acpx"` paths, but Zed's
current ACP ecosystem suggests the higher-value product shape is:

- treat Calciforge as an ACP client for session-aware coding agents,
- preserve per-identity session selection across chat channels,
- expose Calciforge-owned MCP tools for policy, artifacts, secrets, and
  approvals,
- route every ACP agent through the same identity, proxy, Clash policy, and
  audit surface as other adapters,
- use upstream ACP adapters such as `codex-acp` where they are maintained
  separately from editor-specific UI code.

That path fits both direct agents and orchestrators. Orchestrators such as
AgentPool, cagent, or fast-agent can sit behind ACP and present one session
surface to Calciforge while coordinating their own worker teams internally.

### Codex ACP Smoke Notes

`@zed-industries/codex-acp` is a practical first smoke target for native ACP
work. A local protocol smoke against version 0.12.0 initialized cleanly and
advertised:

- `loadSession = true`,
- prompt image support and embedded context support,
- HTTP MCP support,
- auth through ChatGPT, `CODEX_API_KEY`, or `OPENAI_API_KEY`,
- newer `sessionCapabilities.list` and `sessionCapabilities.close` fields.

A raw `session/list` request returned an empty `sessions` array rather than an
error. That means session discovery should not stay limited to the `acpx`
adapter long term. The current native `kind = "acp"` implementation uses the
pinned `sacp` schema, which supports `session/load` but does not expose the
newer `session/list` capability in its typed API. The likely next step is to
either update the ACP/SACP dependency or add a narrow raw JSON-RPC extension for
`session/list` while keeping typed handling for `initialize`, `session/new`,
`session/load`, and `session/prompt`.

Implementation direction:

- preserve `!sessions <agent>` for `acpx`,
- add the same command surface for native ACP agents when they advertise
  `sessionCapabilities.list`,
- store selected native ACP session IDs per identity and agent, as ACPX already
  does for session names,
- load selected sessions with `session/load` instead of creating one global
  adapter session,
- keep channel-facing responses conservative: show title, cwd, updated time,
  and stable session identifier, but do not expose raw protocol metadata by
  default.

## Orchestrator Direction

Orchestrators need a control plane separate from normal chat adapters:

```text
submit task -> receipt -> status/progress -> final outbound message
```

Gas Town is the motivating case. Calciforge should talk to the Mayor by
default, discover available targets, submit or nudge work through normal Gas
Town commands, relay progress, and deliver final summaries or artifacts. Direct
crew or task-worker targeting should be discoverable and policy-gated rather
than treated as ordinary chat routing.

The first repository examples model this as wrapper recipes rather than
first-class adapters:

- `examples/orchestrator-recipes/gastown-sling-stdin` submits stdin to
  `gt sling` and captures a transcript artifact. It requires a real Gas Town
  workspace plus an existing bead or formula.
- `examples/orchestrator-recipes/omo-run-stdin` wraps `oh-my-opencode run
  --json` and captures its structured result artifact. It currently has a
  prompt-in-argv caveat because OmO's CLI accepts the task as a positional
  message.

Useful orchestrator outputs may include:

- status summaries,
- patch bundles,
- test reports,
- screenshots,
- log excerpts,
- trace files,
- generated PR descriptions,
- links to worktrees, branches, or review artifacts.

## Agent-Accessible Calciforge APIs

The artifact model should not be npcsh-specific. npcsh is only the first useful
smoke because image generation naturally produces files. The broader contract
should let an authorized agent or orchestrator ask Calciforge to create, ingest,
validate, and deliver artifacts through the same channel envelope.

OpenClaw is the most important early target because it is a first-class managed
agent path rather than a niche media CLI. The current `openclaw-channel` bridge
sends text into OpenClaw and receives a correlated callback:

```json
{ "sessionKey": "calciforge:librarian:brian", "message": "done" }
```

For diagrams, memes, screenshots, reports, or generated files, the callback can
also include inline base64 attachments while Calciforge still owns the security
boundary:

```json
{
  "sessionKey": "calciforge:librarian:brian",
  "message": "I made a diagram.",
  "attachments": [
    {
      "mimeType": "image/png",
      "name": "diagram.png",
      "caption": "Generated diagram",
      "dataBase64": "..."
    }
  ]
}
```

Calciforge copies callback artifacts into a Calciforge-owned run directory,
enforces type and size limits, and then delivers through Matrix, Telegram, SMS
fallback, or any future channel. URL ingestion remains future work and needs
its own SSRF-safe policy: allowed origins, no ambient credentials, bounded
redirects, content sniffing, byte limits before full reads, and a preference
for local push/upload or short-lived signed URLs over arbitrary fetches.
Callbacks can also carry an `error` field for a correlated request when the
agent or orchestrator has exhausted its reply paths but has no visible result.
That keeps operators out of long timeouts and makes no-reply failures explicit.

There are two complementary integration shapes:

- **Callback ingestion** — OpenClaw, Gas Town, OmO, or another orchestrator
  returns artifact descriptors with final or progress callbacks. This is the
  smallest extension to existing bridges.
- **Agent-callable tools** — Calciforge exposes authenticated local tools such
  as `generate_image`, `attach_file`, `record_progress`, `request_approval`,
  `check_policy`, and `finalize_work`. ACP/MCP agents can call these directly;
  OpenClaw can surface them as native tools; recipe wrappers can call them via
  a local CLI or HTTP endpoint.

The second shape is where generated Starlark and shared SQLite coordination
become useful. Agents should be able to propose glue, but Calciforge should
require operator-reviewed Starlark before changing policy or routing. For
async work, a standard SQLite queue can give agents and orchestrators a durable
coordination surface without forcing every integration to run a bespoke daemon.

## Extension Points

Recipes may eventually need small pieces of glue code rather than only command
and environment configuration. The goal should be constrained extension, not
arbitrary plugin execution.

Possible extension surfaces:

- **Calciforge API callbacks** — agents or orchestrators can post progress,
  artifacts, status changes, approval requests, or final results back to a
  local authenticated Calciforge endpoint.
- **Generated Starlark snippets** — an agent or recipe can propose policy or
  routing glue in Starlark for operator review before Calciforge loads it.
  Generated policy should never be executed silently just because an agent
  produced it.
- **Recipe-local wrappers** — small local scripts can bridge upstream CLIs that
  do not support stdin, stable output paths, or structured status.
- **Shared state files** — a standard SQLite schema could let Calciforge and an
  orchestrator share work queues, receipts, progress events, artifacts, and
  message history without requiring every integration to grow a custom daemon.

A shared SQLite contract is especially interesting for local-first
orchestrators. It could provide a durable, inspectable queue:

```text
work_items(id, target, state, requested_by, created_at, updated_at, prompt)
work_events(id, work_id, kind, body, created_at)
artifacts(id, work_id, path, mime_type, size_bytes, created_at)
approvals(id, work_id, reason, state, created_at, decided_at)
```

The security boundary is important:

- Calciforge should own identity, authorization, and channel delivery.
- Recipes should declare which API endpoints, filesystem paths, artifact types,
  and state tables they need.
- Generated Starlark should be reviewable, versioned, and loaded fail-closed.
- Shared state should use path containment, schema versioning, lock/time-out
  behavior, and audit logs so a stuck or compromised orchestrator cannot wedge
  Calciforge silently.
- Secrets should still flow through `{{secret:NAME}}` references and the
  outbound substitution boundary, not through the shared state file.

## Longer-Term Vision

Text chat remains the baseline, but agents are moving toward richer outputs and
longer-running workflows: generated images, reports, traces, audio/video,
screenshots, resumable jobs, and multi-agent work state. Calciforge should make
those outputs flow through one security and delivery model instead of adding
bespoke behavior for each channel or each upstream tool.

The durable product promise should be: bring your agent runtime, and Calciforge
adds identity, policy, proxying, secrets, artifact hygiene, auditability, and
remote delivery.
