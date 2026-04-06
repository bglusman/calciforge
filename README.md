# 🐾 ZeroClawed

> *The Claw without the scratch.*
> 
> A secure, channel-agnostic agent gateway — declawed for safety, but still sharp where it counts.

---

## 🤔 What is this?

**ZeroClawed** is an agent gateway that lets you chat with AI from **any** channel (Telegram, WhatsApp, Signal, Matrix) while keeping your credentials locked away and your tools sandboxed.

Think of it as a universal remote for AI agents — but one that won't accidentally delete your hard drive because it routes everything through a policy engine first.

### Why "ZeroClawed"?

Because it wraps [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw) with safety features.

- ✅ Wraps the ZeroClaw agent for safety
- ✅ Adds multi-channel support (Telegram, WhatsApp, Signal, Matrix)
- ✅ Routes through credential proxy + policy enforcement
- ❌ Won't run `rm -rf /` because you typo'd "please"

---

## 🚀 Quick Start

```bash
# Clone it
git clone https://github.com/bglusman/zeroclawed
cd zeroclawed

# Build the router
cargo build --release -p zeroclawed

# Build the credential proxy (optional but recommended)
cargo build --release -p onecli-client

# Deploy to your server
./infra/deploy-210.sh --with-zeroclaw --with-claw-code
```

---

## 🏗️ Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      ZeroClawed Router                      │
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
│         ┌────────▼────────┐                                 │
│         │  Clash Policy   │  ← Sandboxing happens here     │
│         └─────────────────┘                                 │
└─────────────────────────────────────────────────────────────┘
```

---

## 🔐 Security First

| Feature | What it does |
|---------|--------------|
| **OneCLI** | Keeps API keys in VaultWarden, not in agent configs |
| **Clash** | Enforces policy on every tool call — no surprise `curl` to shady domains |
| **Identity-aware** | Different users get different agents, different permissions |
| **Unified identity** | Same conversation context across Telegram/WhatsApp/Signal/Matrix |
| **No secrets in repo** | Deploy scripts live in `infra/` (gitignored) |

---

## 🎛️ Configuration

```toml
# /etc/zeroclawed/config.toml

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
bot_token_file = "/etc/zeroclawed/secrets/telegram-token"
enabled = true
```

---

## 🧪 Development

```bash
# Run tests
cargo test

# Run specific crate tests
cargo test -p zeroclawed
cargo test -p onecli-client

# Check formatting
cargo fmt --all -- --check

# Run clippy
cargo clippy --all-targets
```

---

## 📦 Components

| Crate | Purpose |
|-------|---------|
| `zeroclawed` | The main router/gateway binary |
| `onecli-client` | Credential proxy service |
| `host-agent` | System management agent (ZFS, systemd, Proxmox) |
| `outpost` | Content scanning & injection detection |

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

**ZeroClawed** — *Chat safely. Route wisely. Keep your claws retracted.* 🐾
