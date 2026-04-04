# ZeroClawed

A unified Cargo workspace containing the ZeroClawed router and NonZeroClaw native agent, plus their shared crates.

---

## Crates

| Crate | Binary | Description |
|-------|--------|-------------|
| `crates/zeroclawed` | `zeroclawed` | **ZeroClawed** — channel-agnostic router. Owns all inbound channels (Telegram, Matrix, Signal, etc.), enforces auth/allow-lists, and routes messages to downstream agents via outpost-scanned HTTP. |
| `crates/nonzeroclaw` | `nonzeroclaw` | **NonZeroClaw** — opinionated first-party OpenAI-compatible HTTP agent. ZeroClaw fork with outpost scanning, clash policy stubs, web dashboard, Prometheus metrics, and optional hardware/robotics support. |
| `crates/outpost` | *(library)* | **Outpost** — shared content-scanning crate. Detects prompt injection, PII leakage, and unsafe content in external data before it reaches the model context. Used by both zeroclawed and nonzeroclaw. |
| `crates/clash` | *(library)* | **Clash** — policy trait contracts and no-op implementation stubs. Provides the `PolicyEngine` interface for future conflict-resolution / approval-gate features. |
| `crates/robot-kit` | *(library)* | **ZeroClaw Robot Kit** — optional robotics toolkit (drive, vision, speech, sensors, safety monitor). Raspberry Pi / ROS2 target. |

---

## Architecture

```
[Telegram] ──┐
[Matrix]   ──┤──▶ [ZeroClawed boundary] ──▶ [Auth gate] ──▶ [Router] ──▶ [NonZeroClaw]
[Signal]   ──┘          │                                                     │
                    [Outpost]                                             [Outpost]
                  (injection scan)                                      (response scan)
```

- **ZeroClawed** is the sole owner of every inbound channel. No agent connects directly to a channel.
- **Outpost** scans content at both ingress (ZeroClawed) and egress (NonZeroClaw) to catch injection and leakage.
- **Clash** provides a policy layer (currently no-op stubs) for future approval gates and conflict resolution.

---

## Quick Start

### Prerequisites

- Rust 1.87+ (`rustup update stable`)
- SSH access to the deploy target (for remote install)

### Build

```bash
# Check (fast, no codegen):
cargo check

# Debug build:
cargo build

# Optimized release build (both binaries):
cargo build --release -p zeroclawed -p nonzeroclaw

# Release binaries land in:
#   target/release/zeroclawed
#   target/release/nonzeroclaw
```

### Configuration

**ZeroClawed** — config at `~/.zeroclawed/config.toml`:

```toml
version = 2

[[identity]]
id = "brian"
telegram_id = 12345678

[[agent]]
id = "librarian"
url = "http://127.0.0.1:3000"
model = "anthropic/claude-sonnet-4-5"

[[channel.telegram]]
token = "BOT_TOKEN_HERE"
allow_list = ["brian"]
```

**NonZeroClaw** — config at `~/.nonzeroclaw/config.toml` (run `nonzeroclaw config init` to scaffold).

### Deploy

```bash
# Build release binaries:
cargo build --release -p zeroclawed -p nonzeroclaw

# Deploy to a remote host:
scp target/release/zeroclawed user@host:/usr/local/bin/zeroclawed
ssh user@host "systemctl restart zeroclawed"
```

### Agent Adapters

ZeroClawed routes messages to agents via pluggable adapters:

| Adapter | Description | Example agents |
|---------|-------------|----------------|
| `openclaw-http` | OpenAI-compatible HTTP endpoint | NonZeroClaw, OpenClaw proxy, any LLM API |
| `acpx` | Agent Client Protocol via acpx CLI | Claude Code, OpenCode, Kilo, Gemini |
| `cli` | Shell command with `{message}` substitution | Any CLI agent |
| `openclaw-native` | OpenClaw hooks with `deliver:true` | OpenClaw agents (Telegram only) |
| `openclaw-channel` | Bidirectional OpenClaw plugin (experimental) | OpenClaw agents |

---

## Development

```bash
# Lint
cargo clippy --all-targets

# Test
cargo test

# Format
cargo fmt --all
```

---

## Origins

- **ZeroClawed** is a from-scratch rewrite in Rust (originally prototyped in Zig).
- **NonZeroClaw** is a fork of [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw), extended with ZeroClawed-specific features.
- **Outpost** was originally developed as part of ZeroClawed and is now the shared scanning crate for the whole ecosystem.

---

## Roadmap

This is the single source of truth for what we're working on. The internal workboard drives
prioritization from these issues. Contributors: feel free to pick up **[Ready]** items; leave a
comment so others don't duplicate work. **[Design]** items may need maintainer coordination.

### Security

- **[Done] CVE-2026-33579 — clash policy fail-closed by default.**
  Starlark evaluation errors now return `Deny` instead of `Allow`, configurable via `ErrorBehaviour`.
  Host-agent fails closed when `approval_admin_only=true` but `admin_cn_pattern` is not configured.
  See `docs/security-audit-cve-2026-33579.md`.

### Testing

- **[Ready] Loom concurrency testing** — Add `#[cfg(loom)]` tests for data-race and memory-ordering bugs
  that pass on x86 TSO but fail on ARM. CI: `RUSTFLAGS="--cfg loom" cargo test`.
- **[Ready] QEMU cross-architecture testing** — Run tests under `qemu-aarch64` user-mode from x86 CI
  to catch ARM-specific bugs without physical hardware.

### CLI

- **[Ready] Model shortcut aliases** — Quick model switching for mobile.
  Config aliases (`sonnet` → `anthropic/claude-sonnet-4-6`), history navigation (`/model -`
  for last model, `/model -2` for previous), toggle between favorites with just `/model`.
- **[Ready] OpenAI-compatible provider** — Support `OPENAI_API_BASE` style routing
  so users can point at any OpenAI-compatible endpoint (local Ollama, LMStudio, etc.).

### Infrastructure

- **[Ready] PostgreSQL / SQLite session store** — Pluggable persistence for conversation history.
- **[Ready] Prometheus metrics** — Full observability: latency, token usage, error rates
  per provider, model, and identity.

### Agent

- **[Design] ACP agent launcher** — Spawn Claude Code / Codex / Pi as child agents from NZC.
  Needs `acpx` crate fix (Send/Sync trait bounds).
- **[Design] Clash policy engine v2** — Starlark profiles with per-identity policies.
  Core trait is wired; identity-based profile chain is in progress.
- **[Design] Web dashboard** — Admin UI for managing agents, channels, sessions, and policies.

### Channels

- **[Ready] WhatsApp connector** — Full WhatsApp channel support alongside Telegram and Signal.

### RobotKit

- **[Ready] Robot Kit integration** — Drive, vision, sensor toolkit for physical robotics.
  Raspberry Pi / ROS2 target. Crate scaffold exists, needs hardware integration.

### Future

- **[Future] Self-hosted LLM** — Local inference with automatic fallback to cloud models.
- **[Future] Outpost v2** — Multi-layer injection scanning (pattern + semantic + vision
  model) for safer agent-to-agent routing.
- **[Future] Multi-agent orchestration** — Agent-to-agent communication via outpost HTTP.
- **[Future] Edge deployment** — Single binary bundling router + agent for Raspberry Pi /
  container edge nodes without cloud dependency.

---

## License

MIT OR Apache-2.0 (see individual crate `Cargo.toml` for details)
