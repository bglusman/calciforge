# Agent Adapter Notes

Calciforge can dispatch to agents in three broad ways:

- HTTP adapters for long-running services such as OpenClaw or ZeroClaw.
- CLI adapters for one-shot terminal agents such as Codex, Claude Code, Dirac,
  opencode, or local scripts.
- Artifact CLI recipes for one-shot terminal agents that may generate images,
  audio, video, reports, or other files in a Calciforge-controlled run
  directory.
- Exec models for model-gateway calls where the executable owns provider
  authentication and Calciforge only wraps the final text as a chat completion.
- Orchestrators for async work systems such as Gas Town, where Calciforge
  should submit, observe, and relay work rather than pretending every request is
  a synchronous chat completion.

Prefer the smallest stable interface the upstream agent exposes. A documented
JSON or text CLI mode is usually enough for a Calciforge adapter. GUI-only
tools should not become first-class adapters until they expose a scriptable
protocol; otherwise Calciforge inherits the GUI's state model, auth prompts,
and failure modes.

## Current Adapter Posture

| Agent | Calciforge path | Notes |
|---|---|---|
| Codex CLI | `kind = "codex-cli"` or `[[exec_models]]` | Good fit when the Unix account running Calciforge owns Codex credentials. Keep chat-facing agents conservative unless the channel is trusted. |
| Claude Code | `kind = "cli"` or `acpx` | Use `claude -p` for simple subscription-backed prompt execution. Use `acpx` when ACP sessions are needed. |
| OpenClaw | `openclaw-channel` | Preferred path for richer agent runtime, skills, plugins, provider routing, and slash commands. Calciforge no longer supports OpenClaw agent chat through `/v1/chat/completions`. |
| OpenAI-compatible endpoint | `openai-compat` | Plain `/v1/chat/completions` target for Calciforge's model gateway, local test gateways, or compatible model APIs. Set `allow_model_override = true` only when this endpoint should accept Calciforge `!model` selections. |
| Artifact-producing CLI | `kind = "artifact-cli"` | Prototype path for tools such as npcsh media workflows. Calciforge sends the task on stdin, exposes `{artifact_dir}` and `CALCIFORGE_ARTIFACT_DIR`, validates produced files, and returns a text fallback that names attachments without exposing local paths. Telegram and Matrix already use the richer internal envelope; native media upload can be added channel by channel. |
| opencode | `acpx` or generic CLI | Model-agnostic terminal agent with a mature CLI/TUI surface. Prefer ACP when available. |
| Dirac | `kind = "dirac-cli"` | Good scriptable fit. The adapter uses `--yolo --json`, sends the user task on stdin, ignores internal JSON event spam, and returns the final `completion_result`. |
| AgentSwift | Not supported directly | Interesting iOS-specific workflow, but current public shape is a SwiftUI app that drives Claude plus `xcodebuildmcp`/`openspec`, not a stable CLI adapter surface. Revisit if it exposes a noninteractive JSON/ACP/HTTP protocol. |

## Candidate Adapter Findings

These candidates currently look better as recipes or orchestrators than as
hard first-class adapters:

| Candidate | Current fit | Why |
|---|---|---|
| npcsh | `artifact-cli` recipe first | Installs cleanly via `pip install 'npcsh[lite]'`, exposes `npcsh` and `npc`, and has explicit image/video/team commands. The best early value is multimodal artifacts through Calciforge's secured artifact directory. |
| OmO / oh-my-openagent | Orchestrator recipe | Installs through `bunx oh-my-opencode`; the `run` command has noninteractive flags, `--json`, `--on-complete`, `--session-id`, and agent/model overrides. It is primarily an OpenCode harness, so Calciforge should wrap it as async work rather than owning its internals. |
| Gas Town | Orchestrator recipe, then API/state integration | The `gt` CLI exposes Mayor, convoy, sling, feed, dashboard, callbacks, and status commands. Calciforge should talk to Mayor or a work/status API and relay progress/artifacts rather than treating Gas Town as a one-shot chat agent. |

Ironclaw, nullclaw, and similar smaller tools should start as documented
recipes unless they expose a stable protocol that needs Calciforge-specific
parsing or safety behavior.

## Recipes vs Adapters vs Orchestrators

Use a recipe when an upstream tool is useful but its protocol is still just a
documented command invocation. Recipes can still be security-aware: Calciforge
owns identity checks, routing, proxy environment, per-run artifact directories,
timeouts, output validation, and audit logs.

Use a named adapter when Calciforge needs upstream-specific parsing or safety
defaults that cannot be expressed cleanly as a recipe. Dirac is the current
example: it has JSON events, a final completion event, and approval-mode
pitfalls that are worth encoding once.

Use an orchestrator when the upstream owns async work state. Gas Town is the
best candidate: Calciforge should talk to the Mayor by default, submit work,
poll or receive progress, and relay final summaries/artifacts. Direct crew or
polecat targeting should be discoverable and policy-gated rather than treated
as ordinary chat routing.

## Artifact CLI Prototype

`kind = "artifact-cli"` is the first secured recipe path for multimodal tools
such as npcsh. It is intentionally conservative:

- The user task is written to stdin.
- `{message}` in argv is replaced with a fixed instruction to read stdin,
  avoiding prompt leakage through process listings.
- `{artifact_dir}` is replaced with a per-run directory under the local temp
  tree, and the same path is exposed as `CALCIFORGE_ARTIFACT_DIR`.
- Calciforge rejects artifacts that escape the run directory or exceed the
  current size limit.
- Telegram and Matrix receive an internal outbound message envelope; current
  rendering is text fallback with attachment names and sizes, ready for native
  media send support.

Generic artifact recipe:

```toml
[[agents]]
id = "media-recipe"
kind = "artifact-cli"
command = "/path/to/media-agent"
args = [
  "--output",
  "{artifact_dir}/result.png",
  "{message}",
]
timeout_ms = 120000
env = { HTTP_PROXY = "http://127.0.0.1:8888", HTTPS_PROXY = "http://127.0.0.1:8888", NO_PROXY = "localhost,127.0.0.1,::1" }
```

The command above must read the actual task from stdin. `{message}` is a safe
argv marker that expands to "Read the user task from stdin.", not to the
request text.

npcsh image-generation recipe sketch:

```toml
[[agents]]
id = "npcsh-image"
kind = "artifact-cli"
command = "/usr/local/bin/npcsh-vixynt-stdin"
args = [
  "{artifact_dir}/image.png",
]
timeout_ms = 180000
env = { HTTP_PROXY = "http://127.0.0.1:8888", HTTPS_PROXY = "http://127.0.0.1:8888", NO_PROXY = "localhost,127.0.0.1,::1" }
```

Treat this npcsh command as a recipe to verify against the installed npcsh
version; the contract Calciforge provides is the secured stdin/artifact
wrapper, not a guarantee that every npcsh subcommand has stable flags or
stdin-native prompting. If the installed npcsh command only accepts prompts in
argv, use a local wrapper and document that weaker process-listing tradeoff in
the recipe.

Before promoting a recipe, run:

```bash
scripts/agent-recipe-smoke.sh
```

The smoke script installs npcsh, OmO/oh-my-opencode, and Gas Town in disposable
Docker containers and verifies their current noninteractive CLI surfaces. It
does not authenticate providers or run paid model calls.

## OpenClaw Integration Findings

OpenClaw exposes several surfaces that look similar but behave differently:

- `POST /v1/chat/completions` is synchronous and OpenAI-compatible when
  `gateway.http.endpoints.chatCompletions.enabled = true`, but Calciforge does
  not use it for OpenClaw agents. It bypasses the channel/plugin semantics
  required for reliable slash commands and agent identity. Use
  `kind = "openai-compat"` only when you intentionally want a plain model
  endpoint rather than an OpenClaw agent.
- `POST /hooks/agent` is useful for trusted external automations. Current
  OpenClaw releases acknowledge with a `runId` and may execute asynchronously,
  so Calciforge must not treat a bare `{ ok: true, runId }` response as an
  agent reply.
- `POST /calciforge/inbound` plus the Calciforge reply callback is the intended
  channel/plugin style integration. Prefer this when OpenClaw should see
  Calciforge messages as native inbound channel turns.

Operational guidance:

- Keep OpenClaw slash commands as the first token. Calciforge must not prepend
  cross-agent context before `/model`, `/status`, `/reset`, or similar
  downstream commands.
- Treat any remaining `openclaw-http` config as stale and broken. Run
  `calciforge doctor` after install or config edits to catch it before startup.
- Use live OpenClaw gateway tests for command behavior. Mock adapter tests are
  not enough because command parsing depends on enabled gateway endpoints,
  channel/plugin surface, session key shape, and authorization context.

## Dirac Findings

Dirac is attractive for Calciforge because its CLI is scriptable:

```sh
dirac --yolo --json --timeout 120 --cwd /path/to/project \
  "Fix the failing test and summarize the result."
```

Local smoke testing found:

- `dirac --json` can complete a non-edit task and emit a final
  `completion_result`.
- `dirac --yolo --json` can perform a simple edit, run `npm test`, and return a
  concise final answer.
- Non-yolo scripted runs can stop at approval prompts, which is unsuitable for
  unattended Calciforge dispatch.
- JSON output includes repeated internal `api_req_started` events for the same
  request. The Calciforge adapter intentionally ignores those and only returns
  final assistant events.

Operational guidance:

- Keep `--yolo` limited to trusted identities and workspaces.
- Set `timeout_ms` generously for real coding tasks; the adapter still kills the
  child process if it exceeds Calciforge's timeout.
- Prefer prompt-on-stdin configuration. Avoid putting sensitive request text in
  argv.
- If a user reports duplicated replies, inspect whether the upstream CLI emitted
  multiple final assistant events. Internal repeated request events are expected
  noise and should not become channel replies.

## AgentSwift Assessment

AgentSwift appears aimed at "Replit for native Swift" rather than a generic
agent backend. Public documentation describes a SwiftUI app that:

- discovers an Xcode project,
- edits with Claude,
- builds through `xcodebuildmcp`,
- launches and validates in the simulator,
- uses `openspec` to track work.

That workflow is useful to study, especially the Xcode/simulator validation
loop. It is not yet a good Calciforge adapter target because Calciforge needs a
stable process or network interface it can invoke from channel messages.

Better near-term path:

- Document an optional iOS/Xcode recipe using existing `codex-cli`,
  `claude -p`, `opencode`, or `dirac-cli` adapters plus `xcodebuildmcp`.
- Add a first-class AgentSwift adapter only if AgentSwift exposes an ACP, JSON
  CLI, or HTTP mode that can run headlessly and return a machine-readable final
  result.
