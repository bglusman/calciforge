# PolyClaw Monorepo

A unified Cargo workspace containing the PolyClaw router and NonZeroClaw native agent, plus their shared crates.

---

## Crates

| Crate | Binary | Description |
|-------|--------|-------------|
| `crates/polyclaw` | `polyclaw` | **PolyClaw v2** — channel-agnostic router. Owns all inbound channels (Telegram, Matrix, Signal, etc.), enforces auth/allow-lists, and routes messages to downstream agents via outpost-scanned HTTP. |
| `crates/nonzeroclaw` | `nonzeroclaw` | **NonZeroClaw** — opinionated first-party OpenAI-compatible HTTP agent. ZeroClaw fork with outpost scanning, clash policy stubs, web dashboard, Prometheus metrics, and optional hardware/robotics support. |
| `crates/outpost` | *(library)* | **Outpost** — shared content-scanning crate. Detects prompt injection, PII leakage, and unsafe content in external data before it reaches the model context. Used by both polyclaw and nonzeroclaw. |
| `crates/clash` | *(library)* | **Clash** — policy trait contracts and no-op implementation stubs. Provides the `PolicyEngine` interface for future conflict-resolution / approval-gate features. |
| `crates/robot-kit` | *(library)* | **ZeroClaw Robot Kit** — optional robotics toolkit (drive, vision, speech, sensors, safety monitor). Raspberry Pi / ROS2 target. |

---

## Architecture

```
[Telegram] ──┐
[Matrix]   ──┤──▶ [PolyClaw boundary] ──▶ [Auth gate] ──▶ [Router] ──▶ [NonZeroClaw]
[Signal]   ──┘          │                                                     │
                    [Outpost]                                             [Outpost]
                  (injection scan)                                      (response scan)
```

- **PolyClaw** is the sole owner of every inbound channel. No agent connects directly to a channel.
- **Outpost** scans content at both ingress (PolyClaw) and egress (NonZeroClaw) to catch injection and leakage.
- **Clash** provides a policy layer (currently no-op stubs) for future approval gates and conflict resolution.

See the full architecture spec: `/root/.openclaw/workspace/research/polyclaw-v2-spec.md`

---

## Quick Start

### Prerequisites

- Rust 1.87+ (`rustup update stable`)
- SSH access to the deploy target (10.0.0.10)

### Build

```bash
# Check (fast, no codegen):
cargo check

# Debug build:
cargo build

# Optimized release build (both binaries):
cargo build --release -p polyclaw -p nonzeroclaw

# Release binaries land in:
#   target/release/polyclaw
#   target/release/nonzeroclaw
```

### Configuration

**PolyClaw** — config at `~/.polyclaw/config.toml`:

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

### Run

```bash
# Router (foreground):
polyclaw

# Agent (foreground):
nonzeroclaw serve

# As systemd services (on deploy target):
systemctl start polyclaw nonzeroclaw
systemctl enable polyclaw nonzeroclaw
```

---

## Deploy

Build and deploy to the production target (10.0.0.10):

```bash
# Build
cargo build --release -p polyclaw -p nonzeroclaw

# Stop services on target
ssh root@10.0.0.10 "systemctl stop polyclaw nonzeroclaw"

# Copy binaries
scp target/release/polyclaw root@10.0.0.10:/usr/local/bin/polyclaw
scp target/release/nonzeroclaw root@10.0.0.10:/usr/local/bin/nonzeroclaw

# Restart
ssh root@10.0.0.10 "systemctl start polyclaw nonzeroclaw"

# Verify
ssh root@10.0.0.10 "journalctl -u polyclaw -n 5 --no-pager && journalctl -u nonzeroclaw -n 5 --no-pager"
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

- **PolyClaw** is a from-scratch rewrite in Rust (was Zig in v1). See spec for design rationale.
- **NonZeroClaw** is a fork of [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw), extended with PolyClaw-specific features.
- **Outpost** was originally developed as part of PolyClaw v2 and is now the shared scanning crate for the whole ecosystem.

---

## License

MIT OR Apache-2.0 (see individual crate `Cargo.toml` for details)
