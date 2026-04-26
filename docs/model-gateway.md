# Model Gateway

Calciforge can expose an OpenAI-compatible local endpoint while routing
requests across upstream providers, local models, aliases, and synthetic
model choices.

## What Exists Today

| Feature | Status | Notes |
|---|---:|---|
| OpenAI-compatible `/v1/chat/completions` proxy | Working | Local endpoint forwards to configured providers. |
| Provider pattern routing | Working | `[[proxy.providers]]` model globs map model names to upstream APIs. |
| Explicit model routes | Working | `[[proxy.model_routes]]` overrides provider pattern matching. |
| Model shortcuts | Working | `[[model_shortcuts]]` gives users short aliases such as `sonnet`. |
| Local model switching | Working | `[local_models]` manages local `mlx_lm.server` targets. |
| Alloys | Working | `[[alloys]]` samples among interchangeable constituents by `weighted` or `round_robin` strategy, with context-window safety checks. |
| Fallback behavior | Working, implicit | Alloy execution produces an ordered attempt plan; later constituents are tried when earlier ones fail. |
| Named cascades | Working | `[[cascades]]` defines explicit ordered fallback chains and skips targets whose declared context window cannot fit the request. |
| Dispatchers | Working | `[[dispatchers]]` picks the smallest configured context window that fits, then uses larger eligible models as fallbacks. |
| Token estimators | Working | `char_ratio`, `byte_ratio`, and optional `tiktoken-rs` support for OpenAI-compatible BPE counts. |
| Codex/OpenClaw subscription paths | Working | Codex subscription/OAuth usage is exposed as a Calciforge agent path, not as a generic OpenAI-compatible upstream. |

## Synthetic Model Classes

Calciforge uses "synthetic model" to mean "a model name that represents
logic, not a single upstream model ID." There are three intended
classes.

### Alloy

An alloy blends equivalent models. It is useful when any constituent
can answer the request and the operator wants a cost, latency, or
quality mix.

Alloy constituents must be context-compatible. In current code, every
constituent declares `context_window`, and the alloy has an effective
minimum context window. If `min_context_window` is configured, every
constituent must meet or exceed it. If it is omitted, Calciforge
auto-computes the alloy ceiling as the smallest constituent window.
That means mixed-window constituents are allowed only when the alloy
is willing to behave as if it had the smallest window in the group.
For "small request goes local, large request goes remote," use a
dispatcher instead of an alloy.

```toml
[[alloys]]
id = "balanced"
name = "Balanced remote blend"
strategy = "weighted"
min_context_window = 100000

[[alloys.constituents]]
model = "anthropic/claude-sonnet-4.6"
weight = 70
context_window = 200000

[[alloys.constituents]]
model = "openrouter/google/gemini-flash-1.5"
weight = 30
context_window = 100000
```

Current behavior:

- `weighted` samples without replacement for the request.
- `round_robin` rotates the primary constituent.
- every constituent declares `context_window`.
- `min_context_window` is explicit or auto-computed as the minimum
  declared constituent window.
- a constituent below explicit `min_context_window` fails config load.
- the selected model is tried first; remaining constituents become the
  fallback order for that request.

### Cascade

A cascade is an ordered fallback chain: try A, then B, then C on
timeout, 429, 5xx, or other retryable provider failure.

This behavior exists today inside alloy execution, because an alloy
selection returns an ordered list of constituents and the proxy tries
them in order. Named `[[cascades]]` make that behavior explicit
without requiring weighted or round-robin selection. The proxy skips a
cascade target when the request estimate plus output budget exceeds
that target's declared `context_window`.

```toml
[[cascades]]
id = "local-then-remote"
name = "Local first, remote fallback"

[[cascades.models]]
model = "local/qwen3-35b"
context_window = 32768

[[cascades.models]]
model = "anthropic/claude-sonnet-4.6"
context_window = 200000
```

### Dispatcher

A dispatcher chooses a target by request shape. The primary planned
case is "smallest sufficient model": use local/cheap models for small
requests, promote to larger-context or higher-quality models only when
the prompt no longer fits.

The settled name is **dispatcher**, not router, because "router" is
already overloaded by HTTP routing, channel routing, and provider
routing in the codebase.

Dispatchers are implemented as `[[dispatchers]]`. Each target declares
`context_window`; at runtime the gateway estimates the request size,
reserves the requested output budget, and tries the smallest target
that can hold the total. Larger eligible targets become the fallback
order.

```toml
[[dispatchers]]
id = "smart-local"
name = "Use local until the prompt outgrows it"

[[dispatchers.models]]
model = "local/qwen3-35b"
context_window = 32768

[[dispatchers.models]]
model = "openrouter/google/gemini-flash-1.5"
context_window = 100000

[[dispatchers.models]]
model = "anthropic/claude-sonnet-4.6"
context_window = 200000
```

## Config Example

```toml
[proxy]
enabled = true
bind = "127.0.0.1:8080"
backend_type = "http"
backend_url = "https://api.openai.com/v1"
backend_api_key_file = "/etc/calciforge/secrets/openai-key"

[proxy.token_estimator]
strategy = "auto"        # auto, char_ratio, byte_ratio, or tiktoken
# tokenizer = "o200k_base" # optional tiktoken base override for non-OpenAI IDs
safety_margin = 1.10

[[proxy.providers]]
id = "anthropic"
url = "https://api.anthropic.com/v1"
api_key_file = "/etc/calciforge/secrets/anthropic-key"
models = ["claude-*", "anthropic/*"]
timeout_seconds = 120

[[proxy.providers]]
id = "local-mlx"
url = "http://127.0.0.1:8888/v1"
models = ["local/*", "qwen/*", "mlx/*"]

[[proxy.model_routes]]
pattern = "coding/default"
provider = "anthropic"

[[model_shortcuts]]
alias = "sonnet"
model = "anthropic/claude-sonnet-4.6"

[local_models]
enabled = true
current = "qwen3-35b"

[local_models.mlx_lm]
host = "127.0.0.1"
port = 8888

[[local_models.models]]
id = "qwen3-35b"
hf_id = "mlx-community/Qwen2.5-35B-Instruct-8bit"
display_name = "Qwen 35B local"

[[dispatchers]]
id = "smart-local"
name = "Use local until the prompt outgrows it"

[[dispatchers.models]]
model = "local/qwen3-35b"
context_window = 32768

[[dispatchers.models]]
model = "anthropic/claude-sonnet-4.6"
context_window = 200000
```

## Notes

- Codex and Claude subscription-backed CLI routes are agent integrations,
  not generic OpenAI-compatible model-gateway providers. See
  [Codex/OpenClaw integration](codex-openclaw-integration.md) for direct
  `codex-cli`, OpenClaw `openai-codex/*`, OpenClaw `codex/*`, and
  Claude CLI setup choices.
- The model gateway uses a shared `TokenEstimator` trait for fit
  checks. The default `auto` strategy uses `tiktoken-rs` for recognized
  OpenAI-compatible model names when Calciforge is built with
  `--features tiktoken-estimator`, then falls back to the conservative
  char-ratio estimator.
- For non-OpenAI models where an exact provider tokenizer is not
  available, operators can still choose `strategy = "tiktoken"` with
  `tokenizer = "o200k_base"` or `tokenizer = "cl100k_base"` to get a
  real BPE tokenization pass instead of a pure ratio heuristic. Treat
  that as routing-grade, not billing-grade, for Claude, Gemini, Kimi,
  Qwen, or other tokenizer families.
- `char_ratio` and `byte_ratio` remain useful when a deployment wants a
  tiny dependency set or a deliberately conservative approximation for
  code-heavy, mixed-language, or unknown local-model traffic.
- Request-fit checks compare estimated input plus output budget
  against each target's declared context window.
- Provider routes and local model switching are intentionally separate:
  provider routes decide where an OpenAI-style request goes; local
  switching decides which local model process is loaded.
