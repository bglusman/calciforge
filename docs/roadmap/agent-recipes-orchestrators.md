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
- Telegram and Matrix use the internal outbound-message envelope and currently
  render text fallback with attachment names and sizes.

## Near-Term Work

- Add native outbound media sending for Telegram and Matrix.
- Add retention and cleanup policy for artifact directories.
- Add recipe examples for npcsh, opencode/OmO profiles, and other local agent
  CLIs after smoke-testing installed versions.
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

Useful orchestrator outputs may include:

- status summaries,
- patch bundles,
- test reports,
- screenshots,
- log excerpts,
- trace files,
- generated PR descriptions,
- links to worktrees, branches, or review artifacts.

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
