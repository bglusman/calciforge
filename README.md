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
| **Identity-aware** | Different agents get different policies |
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

## 📋 Roadmap & Architecture

### Components

| Crate | Binary | Purpose |
|-------|--------|---------|
| `zeroclawed` | `zeroclawed` | **Router** — channel-agnostic gateway. Owns all inbound channels (Telegram, Matrix, Signal, WhatsApp), enforces auth/allow-lists, and routes messages to downstream agents |
| `onecli-client` | `onecli` | **Credential Proxy** — VaultWarden integration, injects API keys without exposing them to agents |
| `host-agent` | `host-agent` | **System Agent** — ZFS, systemd, Proxmox operations with approval gates |
| `outpost` | *(library)* | **Content Scanner** — detects prompt injection, PII leakage, unsafe content |
| `clash` | *(library)* | **Policy Engine** — sandboxing and tool restrictions |

### Message Flow

```
[Telegram] ──┐
[Matrix]   ──┤──▶ [ZeroClawed] ──▶ [Auth] ──▶ [Router] ──▶ [Agent]
[Signal]   ──┘        │                                    │
[WhatsApp] ──┘   [Outpost scan]                      [OneCLI proxy]
                                                           │
                                                    [VaultWarden]
```

### OneCLI: Universal Secret Proxy

OneCLI can proxy **any** HTTP request with credential injection:

```bash
# LLM APIs (auto-injected)
/proxy/anthropic → api.anthropic.com + Authorization header
/proxy/openai    → api.openai.com + Authorization header
/proxy/kimi      → api.moonshot.cn + Authorization header

# Any secret (explicit lookup)
/vault/Brave%20Search%20API → returns {token: "..."}
/vault/MAM                   → returns {token: "..."}
/vault/Any%20Service         → returns {token: "..."}
```

Agents use OneCLI transparently — the wrapper scripts set the proxy URL, agents make normal requests.

---

**ZeroClawed** — *Chat safely. Route wisely. Keep your claws retracted.* 🐾
