# Agent Recipe Examples

These examples are starting points for local `kind = "artifact-cli"` recipes.
They are intentionally not installed by default: local agent credentials,
models, image backends, and security policy should stay operator-owned.

The stable Calciforge contract for these recipes is:

- Calciforge sends the user task on stdin.
- Calciforge creates a per-run artifact directory and exposes it as
  `CALCIFORGE_ARTIFACT_DIR`.
- Recipes should write generated files only inside that directory.
- Calciforge validates artifact paths and sizes before a channel sees them.
- Sensitive prompt text should not be placed in argv unless the upstream tool
  gives no safer interface.

## npcsh Text Recipe

`npcsh-npc-stdin` reads stdin, sends the task to `npc`, captures npcsh's noisy
output in a transcript artifact, and returns the last meaningful assistant line
as chat text.

Security caveat: the current npcsh `npc` CLI accepts the prompt as a positional
argument, so this wrapper must pass the task through argv after reading stdin.
Use it for local trusted channels and non-sensitive prompts until npcsh exposes
a stdin, file, JSON, or library API path that avoids process-listing leakage.

Example agent config:

```toml
[[agents]]
id = "npcsh"
kind = "artifact-cli"
command = "/opt/calciforge/examples/agent-recipes/npcsh-npc-stdin"
timeout_ms = 180000
aliases = ["npc"]

[agents.env]
NPCSH_CHAT_MODEL = "qwen3.6:27b"
NPCSH_CHAT_PROVIDER = "ollama"
```

Run the wrapper directly before wiring it into Calciforge:

```bash
tmp="$(mktemp -d)"
CALCIFORGE_ARTIFACT_DIR="$tmp" \
  examples/agent-recipes/npcsh-npc-stdin <<'EOF'
Reply exactly: npcsh recipe smoke
EOF
find "$tmp" -maxdepth 1 -type f -print
```

## npcsh Image Recipe

`npcsh-image-stdin` uses npcsh's `vixynt` jinx to create one image in the
artifact directory. It requires explicit image generation config because npcsh
defaults may point at a model that is not installed locally.

Security caveat: `vixynt` currently receives `prompt=...` as a CLI argument.
This has the same process-listing caveat as the text recipe.

This recipe has been smoke-tested on macOS with Ollama 0.22.0 and
`x/z-image-turbo`. That local Ollama model is about 12 GB on disk and produced
a real PNG artifact through npcsh. In that smoke, Ollama returned a 1024x1024
image even when the wrapper requested 512x512, so treat width and height as
best-effort knobs until the upstream image API stabilizes.

Example agent config:

```toml
[[agents]]
id = "npcsh-image"
kind = "artifact-cli"
command = "/opt/calciforge/examples/agent-recipes/npcsh-image-stdin"
timeout_ms = 180000
aliases = ["image"]

[agents.env]
NPCSH_IMAGE_GEN_MODEL = "x/z-image-turbo"
NPCSH_IMAGE_GEN_PROVIDER = "ollama"
CALCIFORGE_NPCSH_TIMEOUT_SECONDS = "120"
```

Install the local Ollama image model before using the recipe:

```bash
ollama pull x/z-image-turbo
```

For artifact pipeline testing without a real npcsh image backend, set
`CALCIFORGE_NPCSH_IMAGE_DEMO=1`. Demo mode creates a deterministic PNG and a
text note containing the prompt, so use it only for non-sensitive local smoke
tests.

```bash
tmp="$(mktemp -d)"
CALCIFORGE_ARTIFACT_DIR="$tmp" \
CALCIFORGE_NPCSH_IMAGE_DEMO=1 \
  examples/agent-recipes/npcsh-image-stdin <<'EOF'
A small red square icon for an artifact smoke test.
EOF
file "$tmp"/npcsh-vixynt.png
```

## Promotion Checklist

Before a recipe becomes recommended documentation:

- Verify installability in `scripts/agent-recipe-smoke.sh`.
- Smoke the wrapper against an installed local target.
- Confirm failures are explicit and nonzero.
- Confirm no prompt text leaks through argv when avoidable.
- Confirm artifacts are useful on Telegram and Matrix text fallback paths.
- Add native channel media tests when the channel supports upload.
