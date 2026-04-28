# Agent Adapter Notes

Calciforge can dispatch to agents in three broad ways:

- HTTP adapters for long-running services such as OpenClaw or ZeroClaw.
- CLI adapters for one-shot terminal agents such as Codex, Claude Code, Dirac,
  opencode, or local scripts.
- Exec models for model-gateway calls where the executable owns provider
  authentication and Calciforge only wraps the final text as a chat completion.

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
| OpenClaw | `openclaw-native`, `openclaw-http`, or model gateway upstream | Preferred path for richer agent runtime, skills, plugins, and provider routing. |
| opencode | `acpx` or generic CLI | Model-agnostic terminal agent with a mature CLI/TUI surface. Prefer ACP when available. |
| Dirac | `kind = "dirac-cli"` | Good scriptable fit. The adapter uses `--yolo --json`, sends the user task on stdin, ignores internal JSON event spam, and returns the final `completion_result`. |
| AgentSwift | Not supported directly | Interesting iOS-specific workflow, but current public shape is a SwiftUI app that drives Claude plus `xcodebuildmcp`/`openspec`, not a stable CLI adapter surface. Revisit if it exposes a noninteractive JSON/ACP/HTTP protocol. |

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
