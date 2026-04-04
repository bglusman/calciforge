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

See the full architecture spec in `/docs` or run `zeroclawed install --help` for the guided setup.

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

### Install (interactive wizard)

ZeroClawed includes a built-in installer that configures routing, agents, channels, and systemd services on local or remote hosts.

```bash
# Interactive TUI wizard (default):
zeroclawed install

# Headless / non-interactive:
zeroclawed install --zeroclawed-host <target> --claw <agent-spec>

# Dry-run (prints planned changes, touches nothing):
zeroclawed install --dry-run
```

The installer is **idempotent** — safe to re-run. It:
- Backs up configs before any write
- Health-checks after apply
- Auto-rolls back on failure
- Supports SSH-based remote configuration of NZC and OpenClaw targets

See `zeroclawed install --help` for all options.

### Run

```bash
# Router (foreground):
zeroclawed

# Agent (foreground):
nonzeroclaw serve

# As systemd services:
systemctl start zeroclawed nonzeroclaw
systemctl enable zeroclawed nonzeroclaw
```

### Agent adapters

ZeroClawed routes messages to agents via pluggable adapters:

| Adapter | Description | Example agents |
|---------|-------------|----------------|
| `openclaw-http` | OpenAI-compatible HTTP endpoint | NonZeroClaw, OpenClaw proxy, any LLM API |
| `acpx` | Agent Client Protocol via acpx CLI | Claude Code, OpenCode, Kilo, Gemini |
| `cli` | Shell command with `{message}` substitution | Any CLI agent |
| `openclaw-native` | OpenClaw hooks with `deliver:true` | OpenClaw agents (Telegram only) |
| `openclaw-channel` | Bidirectional OpenClaw plugin (experimental) | OpenClaw agents |

See [docs/acpx-claude-setup.md](docs/acpx-claude-setup.md) for Claude Code integration guide.

---

## Deploy

```bash
# Build release binaries:
cargo build --release -p zeroclawed -p nonzeroclaw

# Deploy to a remote host:
scp target/release/zeroclawed user@host:/usr/local/bin/zeroclawed
ssh user@host "systemctl restart zeroclawed"
```

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

- **ZeroClawed** is a from-scratch rewrite in Rust (originally prototyped in Zig). See spec for design rationale.
- **NonZeroClaw** is a fork of [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw), extended with ZeroClawed-specific features.
- **Outpost** was originally developed as part of ZeroClawed and is now the shared scanning crate for the whole ecosystem.

---

## License
---

## Roadmap

> This is the single source of truth for what we're working on. GitHub issues are tracked here;
> the internal workboard drives prioritization from these items.
> Contributors: feel free to pick up any **[🟢 Ready]** item — just leave a comment so others
> don't duplicate work. **[🚧 In Progress]** or **[📐 Design]** items may need coordination with
> maintainers first.

### 🟢 Ready

### 🔐 Security
- **CVE-2026-33579 fix — Clash policy fail-closed** ✅ Merged
  - Starlark evaluation errors now `Deny` by default (was `Allow`)
  - `admin_cn_pattern` missing now fails closed in host-agent
  - Report: `docs/security-audit-cve-2026-33579.md`

### 🧪 Testing
- **Loom concurrency testing** — Add `#[cfg(loom)]` tests to clash and NZC for
  memory ordering, data races, and deadlocks that pass on x86 TSO but fail on ARM.
  CI: `RUSTFLAGS="--cfg loom" cargo test`
- **QEMU cross-architecture testing** — Run NZC test suite under `qemu-aarch64`
  user-mode from x86 CI. Catch ARM-specific bugs without physical hardware.
  Also useful for endianness (s390x, PowerPC) if RobotKit targets embedded.

### 🛠 CLI Improvements
- **Model shortcut aliases** — Quick model switching for mobile users.
  Config-defined aliases (`!sc sonnet` → `anthropic/claude-sonnet-4.6`),
  history navigation (`/model -` for last model, `/model -2` for previous),
  toggle between favorites with just `/model`.
- **OpenAI-compatible provider** — Support `OPENAI_API_BASE` style routing
  so users can point at any OpenAI-compatible endpoint (local Ollama, LMStudio, etc.).

### 📦 Infrastructure
- **PostgreSQL / SQLite session store** — Pluggable persistence for conversation history.
- **Prometheus metrics** — Full observability: latency, token usage, error rates per
  provider, model, and identity.

### 📐 Design In Progress

### 🤖 Agent Features
- **ACP agent launcher** — Spawn Claude Code / Codex / Pi as child agents from NZC.
  Already has `acp.rs` provider but needs `acpx` crate fix (Send/Sync traits).
- **Clash policy engine v2** — Starlark profiles with per-identity policies.
  Core trait is wired; identity-based profile chain is in progress.
- **Web dashboard** — Admin UI for managing agents, channels, sessions, and policies.

### 🌐 Channels
- **WhatsApp connector** — Full WhatsApp channel support alongside Telegram and Signal.
- **Self-hosted LLM** — Local inference with automatic fallback to cloud models.

### 🤖 RobotKit
- **Robot Kit integration** — Drive + vision + sensor toolkit for physical robotics.
  Raspberry Pi / ROS2 target. Robot Kit crate exists, needs hardware integration docs.

### 🔮 Future
- **Outpost v2** — Multi-layer injection scanning (pattern + semantic + vision)
  for safer agent-to-agent routing.
- **Multi-agent orchestration** — Agent-to-agent communication via outpost HTTP.
  Agent mesh networking with per-agent auth (already in auth.rs).
- **Edge deployment** — Single binary that bundles router + agent for
  Raspberry Pi / container edge nodes without cloud dependency.

---

## License

MIT OR Apache-2.0 (see individual crate `Cargo.toml` for details)
