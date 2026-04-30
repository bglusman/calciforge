# Orchestrator Recipe Examples

Orchestrator recipes are for tools that own async work state. Calciforge should
submit work, return a receipt or status summary, and later relay progress or
final artifacts. They are not ordinary chatbots.

The long-term Calciforge shape should be:

```text
submit task -> receipt -> status/progress -> final outbound message
```

These wrappers are starting points for operators and agents to adapt. They do
not imply first-class support or default installation.

## Gas Town

Gas Town's `gt sling` is the best current command surface. It supports
`--stdin`, target routing, agent overrides, merge strategies, dry runs, and
auto-convoy creation.

Surface smoke:

```bash
npx --yes @gastown/gt --help
npx --yes @gastown/gt sling --help
```

Current package metadata from npm:

- package: `@gastown/gt`
- tested CLI version: `0.12.0`
- binary: `gt`

Example config:

```toml
[[agents]]
id = "gastown-mayor"
kind = "artifact-cli"
command = "/opt/calciforge/examples/orchestrator-recipes/gastown-sling-stdin"
timeout_ms = 60000
aliases = ["mayor", "gastown"]

[agents.env]
CALCIFORGE_GASTOWN_WORK = "gt-abc"
CALCIFORGE_GASTOWN_TARGET = "mayor"
CALCIFORGE_GASTOWN_MERGE = "local"
```

`CALCIFORGE_GASTOWN_WORK` must be an existing bead or formula in the configured
Gas Town workspace. In a real integration, Calciforge should store the returned
convoy/bead/session identifiers in a work queue and expose a status command
rather than waiting synchronously for completion.

Use dry-run mode for setup checks:

```bash
tmp="$(mktemp -d)"
CALCIFORGE_ARTIFACT_DIR="$tmp" \
CALCIFORGE_GASTOWN_WORK="gt-abc" \
CALCIFORGE_GASTOWN_TARGET="mayor" \
CALCIFORGE_GASTOWN_DRY_RUN=1 \
  examples/orchestrator-recipes/gastown-sling-stdin <<'EOF'
Review this issue and propose a plan.
EOF
```

This repository is not a Gas Town workspace, so local smoke here can verify the
CLI surface but not a full work dispatch.

## Oh My OpenAgent / Oh My OpenCode

OmO exposes a useful noninteractive `run` command with JSON output, session
resumption, model/agent overrides, an attach URL, and an `--on-complete` hook.
It waits for todos and background sessions to become idle, so it behaves like a
small orchestrator around OpenCode.

Surface smoke:

```bash
npx --yes oh-my-opencode --help
npx --yes oh-my-opencode run --help
```

Current package metadata from npm:

- package: `oh-my-opencode`
- tested CLI version: `3.17.12`
- binaries: `oh-my-opencode`, `oh-my-openagent`

Example config:

```toml
[[agents]]
id = "omo"
kind = "artifact-cli"
command = "/opt/calciforge/examples/orchestrator-recipes/omo-run-stdin"
timeout_ms = 600000
aliases = ["omo", "oh-my-openagent"]

[agents.env]
CALCIFORGE_OMO_AGENT = "Sisyphus"
CALCIFORGE_OMO_DIRECTORY = "/path/to/workspace"
```

Important caveat: `oh-my-opencode run` currently takes the task as a positional
argv message. The wrapper reads stdin for a uniform Calciforge contract, but it
must pass that message to OmO in argv. Do not use it for sensitive prompts until
OmO exposes a stdin or file-backed message mode.

The wrapper stores structured JSON in `omo-result.json` when `--json` succeeds.
