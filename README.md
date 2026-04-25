# 🐾 Calciforge

> *The Claw without the scratch.*
> 
> A secure, channel-agnostic agent gateway — declawed for safety, but still sharp where it counts.

---

## 🤔 What is this?

**Calciforge** is an agent gateway that lets you chat with AI from **any** channel (Telegram, WhatsApp, Signal, Matrix) while keeping your credentials locked away and your tools sandboxed.

Think of it as a universal remote for AI agents — but one that won't accidentally delete your hard drive because it routes everything through a policy engine first.

### Why "Calciforge"?

Because it wraps [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw) with safety features.

- ✅ Wraps the ZeroClaw agent for safety
- ✅ Adds multi-channel support (Telegram, WhatsApp, Signal, Matrix)
- ✅ Routes through credential proxy + policy enforcement
- ❌ Won't run `rm -rf /` because you typo'd "please"

---

## 🚀 Quick Start

```bash
# Clone it
git clone https://github.com/bglusman/calciforge
cd calciforge

# Build the router
cargo build --release -p calciforge

# Build the credential proxy (optional but recommended)
cargo build --release -p secrets-client

# Deploy to your server
./infra/deploy-210.sh --with-zeroclaw --with-claw-code
```

---

## 🏗️ Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      Calciforge Router                      │
│  ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────────────┐  │
│  │Telegram │ │WhatsApp │ │ Signal  │ │ Matrix          │  │
│  └────┬────┘ └────┬────┘ └────┬────┘ └────────┬────────┘  │
│       └─────────────┴───────────┴────────────────┘          │
│                         │                                   │
│              ┌──────────▼──────────┐                        │
│              │   Message Router    │                        │
│              └──────────┬──────────┘                        │
│                         │                                   │
│       ┌─────────────────┼─────────────────┐                 │
│       │                 │                 │                 │
│  ┌────▼────┐      ┌─────▼─────┐    ┌────▼────┐             │
│  │claw-code│      │zeroclawlabs│   │ Any CLI │             │
│  │(Claude) │      │(Kimi/Gemini)│   │  agent  │             │
│  └────┬────┘      └─────┬─────┘    └────┬────┘             │
│       │                 │                 │                 │
│       └──────────┬──────┴─────────────────┘                 │
│                  │                                          │
│         ┌────────▼────────┐                                 │
│         │   OneCLI Proxy  │  ← Credentials live here       │
│         └────────┬────────┘                                 │
│                  │                                          │
│         ┌────────▼────────┐     ┌──────────────────────┐   │
│         │ Policy Plugin   │────▶│       clashd         │   │
│         │ (before_tool_)  │     │  Starlark + Domain   │   │
│         └─────────────────┘     │  Filtering + Threat  │   │
│                                 │  Intel Feeds         │   │
│                                 └──────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

---

## 🔐 Security First

| Feature | What it does |
|---------|--------------|
| **OneCLI** | Keeps API keys in VaultWarden, not in agent configs |
| **clashd** | Centralized Starlark policy engine with domain filtering |
| **Domain Filtering** | Regex patterns, threat intel feeds, per-agent allow/deny lists |
| **Dynamic Threat Intel** | Auto-updates from URLHaus, StevenBlack, custom feeds |
| **Adversary Detector** | Three-layer content scanning: structural → semantic → remote service |
| **Digest Caching** | SHA-256 of response bodies — same content = cache hit, changed = rescan |
| **Skip Protection** | Trusted domains bypass scanning entirely (exact match + `*.domain.com` wildcard) |
| **Security Profiles** | Named presets: open / balanced / hardened / paranoid |
| **Identity-aware** | Different agents get different policies |
| **Unified identity** | Same conversation context across Telegram/WhatsApp/Signal/Matrix |
| **No secrets in repo** | Deploy scripts live in `infra/` (gitignored) |

---

## 🎛️ Configuration

```toml
# /etc/calciforge/config.toml

[[identities]]
id = "brian"
aliases = [
  { channel = "telegram", id = "123456789" },
  { channel = "whatsapp", id = "+12155551234" },
]
role = "owner"

[[agents]]
id = "claw-code"
kind = "cli"
command = "/usr/local/bin/claw-wrapped"
timeout_ms = 120000

[[agents]]
id = "zeroclawlabs"
kind = "cli"  
command = "/usr/local/bin/zeroclaw-wrapped"
timeout_ms = 90000

[[routing]]
identity = "brian"
default_agent = "claw-code"
allowed_agents = ["claw-code", "zeroclawlabs", "librarian"]

[[channels]]
kind = "telegram"
bot_token_file = "/etc/calciforge/secrets/telegram-token"
enabled = true
```

---

## 🔌 AI Model Proxy

Calciforge includes an **OpenAI-compatible HTTP proxy** (`[proxy]`) that routes model requests to one or more backends, with named provider routing, local model management, streaming support, and tool-call forwarding.

### Multi-Provider Routing

Route different model names to different providers — each with its own URL, API key, and timeout:

```toml
[proxy]
enabled = true
bind = "127.0.0.1:8080"
backend_type = "http"
backend_url = "https://api.openai.com/v1"     # default fallback
backend_api_key_file = "/etc/calciforge/secrets/openai-key"

# Named providers — matched in order against incoming model name
# Pattern syntax: exact match, "*" (any), or "prefix/*" (prefix glob)
[[proxy.providers]]
id = "local"
models = ["local/*", "llama/*", "qwen/*", "gemma/*"]
url = "http://localhost:8888/v1"

[[proxy.providers]]
id = "fast-provider"
models = ["fast/*"]
url = "https://api.fast-provider.example.com/v1"
api_key_file = "/etc/calciforge/secrets/fast-key"
timeout_seconds = 30
```

### Model Alloys (Blended Routing)

**Alloy** — inspired by [Alloy: A Model for Blended LLM Outputs](https://arxiv.org/abs/2410.10630) — routes requests across multiple backends for cost efficiency, quality blending, and graceful degradation:

```toml
[[alloys]]
id = "balanced"
strategy = "weighted"

[[alloys.constituents]]
model = "openrouter/google/gemini-flash-1.5"
weight = 80

[[alloys.constituents]]
model = "openrouter/anthropic/claude-3-haiku"
weight = 20
```

Users switch alloys via chat:
```
!model                 # List available models/alloys
!model balanced        # Activate an alloy
```

Strategies: `weighted` (random by weight) · `round_robin` (deterministic cycling)

### Local Model Management

Run models locally via [mlx_lm](https://github.com/ml-explore/mlx-lm) (Apple Silicon) or [llama.cpp](https://github.com/ggerganov/llama.cpp) and switch between them at runtime:

```toml
[local_models]
enabled = true
current = "qwen3-35b"

# mlx_lm.server settings (shared across all models)
[local_models.mlx_lm]
port = 8888
host = "127.0.0.1"

[[local_models.models]]
id = "qwen3-35b"
hf_id = "mlx-community/Qwen2.5-35B-Instruct-8bit"
# provider_type = "mlx_lm"  # default
display_name = "Qwen 3.5 35B"

[[local_models.models]]
id = "gemma4-26b"
hf_id = "mlx-community/gemma-4-26b-it-8bit"
display_name = "Gemma 4 26B"
```

Switch via API:
```bash
curl -X POST http://localhost:8080/control/local/switch \
  -H "Content-Type: application/json" \
  -d '{"model": "gemma4-26b"}'
```

---

## 🎙️ Voice Pipeline

Calciforge provides minimal, **non-opinionated** passthrough endpoints for speech-to-text and text-to-speech. It forwards audio/text to whatever STT/TTS servers you configure — no opinions about VAD, wakeword detection, or pipeline topology.

```toml
[proxy.voice.stt]
url = "http://localhost:9000"          # any OpenAI-compatible STT server
timeout_seconds = 60

[proxy.voice.tts]
url = "http://localhost:9001"          # any OpenAI-compatible TTS server
timeout_seconds = 60

[proxy.voice.hooks]
on_audio_in = "/etc/calciforge/hooks/preprocess-audio.sh"   # optional
on_text_out = "/etc/calciforge/hooks/postprocess-text.sh"   # optional
```

**Endpoints** (always registered; return `501` when not configured):

| Endpoint | Description |
|----------|-------------|
| `POST /v1/audio/transcriptions` | Forward audio to STT, return transcript |
| `POST /v1/audio/speech` | Forward text to TTS, return audio |
| `GET  /v1/tools/manifest` | OpenAI-compatible tool definitions for what's configured |

**Hooks** receive the request body on stdin and write the (optionally transformed) body to stdout. On failure, the original body passes through unchanged — the pipeline degrades gracefully rather than erroring.

The `GET /v1/tools/manifest` endpoint returns tool definitions a model can inject directly into its `tools` parameter: `calciforge_switch_model`, `calciforge_current_model`, `calciforge_transcribe`, `calciforge_speak` — only for features actually configured.

---

## 🛡️ Policy Enforcement (clashd)

clashd is a sidecar service that evaluates every tool call through a Starlark policy before execution.

### Features

- **Starlark Policies**: Turing-complete policy language for complex rules
- **Domain Filtering**: Exact match, regex patterns, subdomain matching
- **Threat Intelligence**: Dynamic feeds from URLHaus, StevenBlack, custom sources
- **Per-Agent Policies**: Different rules for different agents
- **Custodian Approval**: Require human review for sensitive operations

### Quick Start

```bash
# Build and run clashd
cargo build --release -p clashd
CLASHD_POLICY=crates/clashd/config/default-policy.star ./target/release/clashd

# In another terminal, test it
curl -X POST http://localhost:9001/evaluate \
  -H "Content-Type: application/json" \
  -d '{"tool": "exec", "args": {"command": "ls"}, "context": {"agent_id": "test"}}'
```

### Policy Example (`policy.star`)

```python
def evaluate(tool, args, context):
    # Block known-bad domains
    if context.get("domain_lists"):
        return {"verdict": "deny", "reason": "Domain in threat feed"}

    # Require approval for config changes
    if tool == "gateway":
        return {"verdict": "review", "reason": "Config change needs approval"}

    # Block destructive commands
    if tool == "exec" and "rm -rf /" in args.get("command", ""):
        return {"verdict": "deny", "reason": "Destructive command blocked"}

    return "allow"
```

See [crates/clashd/README.md](crates/clashd/README.md) for full documentation.

---

## 🔍 Content Scanning (adversary-detector)

All external content fetched by agents passes through `adversary-detector` — a three-layer scanner with SHA-256 digest caching and skip protection.

### How It Works

1. **Fetch** — proxy fetches the URL over HTTPS
2. **Digest check** — SHA-256 of response body compared to cached entry
   - Same URL + same digest → cached verdict, no rescan (fast path)
   - New or changed digest → full scan pipeline runs
3. **Three-layer scan:**
   - Layer 1 (structural): zero-width chars, unicode tag hiding, base64 blobs
   - Layer 2 (semantic): injection phrases, PII harvesting signals, exfiltration patterns
   - Layer 3 (remote): optional deeper analysis via shared HTTP service
4. **Verdict:** Clean / Review / Unsafe → returned to caller

### Skip Protection (Trusted Domains)

Domains in `skip_protection_domains` bypass scanning entirely:

```toml
# In scanner config
skip_protection_domains = [
    "api.internal.example.com",    # exact match
    "*.trusted-cdn.com",           # wildcard — all subdomains
]
```

Skip protection is distinct from digest caching: digest caching scans first then caches; skip protection never scans at all. Use for domains you fully control.

### Security Profiles

Four named presets control scanning depth, rate limits, and logging:

| Profile | Scans | Discussion Ratio | Review | Rate |
|---------|-------|-----------------|--------|------|
| **Open** | web_fetch only | 0.5 | auto-pass | 120/min |
| **Balanced** | web + search | 0.3 | needs approval | 60/min |
| **Hardened** | all tools | 0.15 | blocked | 30/min |
| **Paranoid** | all + exec | 0.0 | blocked | 15/min |

See [crates/adversary-detector/README.md](crates/adversary-detector/README.md) for full documentation.

---

## 🧪 Development

```bash
# Run tests
cargo test

# Run specific crate tests
cargo test -p calciforge
cargo test -p secrets-client

# Check formatting
cargo fmt --all -- --check

# Run clippy
cargo clippy --all-targets
```

---

## 📦 Components

| Crate | Purpose |
|-------|---------|
| `calciforge` | The main router/gateway binary |
| `secrets-client` | Credential proxy service |
| `host-agent` | System management agent (ZFS, systemd, Proxmox) |
| `adversary-detector` | Content scanning, digest caching, skip protection |
| `clashd` | Starlark policy engine with domain filtering and threat intel |

---

## 🤝 Related Projects

- **[ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw)** — The upstream agent framework
- **[claw-code](https://github.com/instructkr/claw-code)** — Claude Code integration
- **[clash](https://crates.io/crates/clash)** — Policy enforcement engine

---

## 📝 License

MIT — See [LICENSE](LICENSE)

---

## 🙏 Acknowledgments

Built with:
- ☕ Too much coffee
- 🦀 Rust's borrow checker (our enemy and our friend)
- 🤖 A healthy fear of un-sandboxed AI agents

> *"The best code is code that doesn't accidentally delete your home directory."*
> — Ancient Proverb

---

## 📋 Roadmap & Architecture

### Components

| Crate | Binary | Purpose |
|-------|--------|---------|
| `calciforge` | `calciforge` | **Router** — channel-agnostic gateway. Owns all inbound channels (Telegram, Matrix, Signal, WhatsApp), enforces auth/allow-lists, and routes messages to downstream agents. Includes OpenAI-compatible model proxy with multi-provider routing, local model management, and voice pipeline passthrough. |
| `secrets-client` | `secrets` | **Credential Proxy** — VaultWarden integration, injects API keys without exposing them to agents |
| `host-agent` | `host-agent` | **System Agent** — ZFS, systemd, Proxmox operations with approval gates |
| `adversary-detector` | *(library)* | **Content Scanner** — three-layer detection, digest caching, skip protection, security profiles — [README](crates/adversary-detector/README.md) |
| `clashd` | `clashd` | **Policy Engine** — Starlark policies, domain filtering, threat intel feeds, per-agent configs — [README](crates/clashd/README.md) |

### Message Flow

```
[Telegram] ──┐
[Matrix]   ──┤──▶ [Calciforge] ──▶ [Auth] ──▶ [Router] ──▶ [Agent]
[Signal]   ──┘        │                                    │
[WhatsApp] ──┘   [adversary-detector]               [OneCLI proxy]
                                                           │
                                                    [VaultWarden]
```

### OneCLI: Universal Secret Proxy

OneCLI can proxy **any** HTTP request with credential injection:

```bash
# LLM APIs (auto-injected)
/proxy/anthropic → api.anthropic.com + Authorization header
/proxy/openai    → api.openai.com + Authorization header

# Any secret (explicit lookup)
/vault/Brave%20Search%20API → returns {token: "..."}
/vault/Any%20Service         → returns {token: "..."}
```

Agents use OneCLI transparently — the wrapper scripts set the proxy URL, agents make normal requests.

---

**Calciforge** — *Chat safely. Route wisely. Keep your claws retracted.* 🐾
