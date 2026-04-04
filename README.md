# NonZeroClawed

A unified Cargo workspace containing the NonZeroClawed router and NonZeroClaw native agent, plus their shared crates.

---

## Crates

| Crate | Binary | Description |
|-------|--------|-------------|
| `crates/nonzeroclawed` | `nonzeroclawed` | **NonZeroClawed v2** — channel-agnostic router. Owns all inbound channels (Telegram, Matrix, Signal, etc.), enforces auth/allow-lists, and routes messages to downstream agents via outpost-scanned HTTP. |
| `crates/nonzeroclaw` | `nonzeroclaw` | **NonZeroClaw** — opinionated first-party OpenAI-compatible HTTP agent. ZeroClaw fork with outpost scanning, clash policy stubs, web dashboard, Prometheus metrics, and optional hardware/robotics support. |
| `crates/outpost` | *(library)* | **Outpost** — shared content-scanning crate. Detects prompt injection, PII leakage, and unsafe content in external data before it reaches the model context. Used by both nonzeroclawed and nonzeroclaw. |
| `crates/clash` | *(library)* | **Clash** — policy trait contracts and no-op implementation stubs. Provides the `PolicyEngine` interface for future conflict-resolution / approval-gate features. |
| `crates/robot-kit` | *(library)* | **ZeroClaw Robot Kit** — optional robotics toolkit (drive, vision, speech, sensors, safety monitor). Raspberry Pi / ROS2 target. |

---

## Architecture

```
[Telegram] ──┐
[Matrix]   ──┤──▶ [NonZeroClawed boundary] ──▶ [Auth gate] ──▶ [Router] ──▶ [NonZeroClaw]
[Signal]   ──┘          │                                                     │
                    [Outpost]                                             [Outpost]
                  (injection scan)                                      (response scan)
```

- **NonZeroClawed** is the sole owner of every inbound channel. No agent connects directly to a channel.
- **Outpost** scans content at both ingress (NonZeroClawed) and egress (NonZeroClaw) to catch injection and leakage.
- **Clash** provides a policy layer (currently no-op stubs) for future approval gates and conflict resolution.

See the full architecture spec in `/docs` or run `nonzeroclawed install --help` for the guided setup.

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
cargo build --release -p nonzeroclawed -p nonzeroclaw

# Release binaries land in:
#   target/release/nonzeroclawed
#   target/release/nonzeroclaw
```

### Configuration

**NonZeroClawed** — config at `~/.nonzeroclawed/config.toml`:

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

NonZeroClawed includes a built-in installer that configures routing, agents, channels, and systemd services on local or remote hosts.

```bash
# Interactive TUI wizard (default):
nonzeroclawed install

# Headless / non-interactive:
nonzeroclawed install --nonzeroclawed-host <target> --claw <agent-spec>

# Dry-run (prints planned changes, touches nothing):
nonzeroclawed install --dry-run
```

The installer is **idempotent** — safe to re-run. It:
- Backs up configs before any write
- Health-checks after apply
- Auto-rolls back on failure
- Supports SSH-based remote configuration of NZC and OpenClaw targets

See `nonzeroclawed install --help` for all options.

### Run

```bash
# Router (foreground):
nonzeroclawed

# Agent (foreground):
nonzeroclaw serve

# As systemd services:
systemctl start nonzeroclawed nonzeroclaw
systemctl enable nonzeroclawed nonzeroclaw
```

### Agent adapters

NonZeroClawed routes messages to agents via pluggable adapters:

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
cargo build --release -p nonzeroclawed -p nonzeroclaw

# Deploy to a remote host:
scp target/release/nonzeroclawed user@host:/usr/local/bin/nonzeroclawed
ssh user@host "systemctl restart nonzeroclawed"
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

- **NonZeroClawed** is a from-scratch rewrite in Rust (was Zig in v1). See spec for design rationale.
- **NonZeroClaw** is a fork of [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw), extended with NonZeroClawed-specific features.
- **Outpost** was originally developed as part of NonZeroClawed v2 and is now the shared scanning crate for the whole ecosystem.

---

## License

MIT OR Apache-2.0 (see individual crate `Cargo.toml` for details)
