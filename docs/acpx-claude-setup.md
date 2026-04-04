# Claude Code via acpx — Setup Guide

NonZeroClawed supports routing messages to [Claude Code](https://code.claude.com) via the `acpx` CLI adapter. This lets you chat with Claude through WhatsApp, Signal, Telegram, or any other NonZeroClawed channel.

## How it works

```
[WhatsApp/Signal/Telegram]
        ↓
   [NonZeroClawed router]
        ↓
  [acpx adapter]  →  acpx CLI  →  Claude Code (claude binary)
        ↓
   [reply sent back to channel]
```

The `acpx` adapter calls `acpx <agent> exec <message>` for each inbound message. `acpx` manages Claude Code sessions and handles the ACP protocol translation.

## Prerequisites

- Node.js 18+
- `acpx` CLI: `npm install -g acpx`
- Claude Code: `npm install -g @anthropic-ai/claude-code` (or via official installer)
- A Claude subscription (claude.ai) or API key

## Installation

### 1. Install acpx and Claude Code

```bash
npm install -g acpx
npm install -g @anthropic-ai/claude-code
```

> **Note:** Claude Code cannot run as root. If NonZeroClawed runs as root, create a dedicated user:
> ```bash
> useradd -m -s /bin/bash claude
> su - claude -c 'npm install -g @anthropic-ai/claude-code'
> ```
> Then create `/usr/local/bin/claude` wrapper:
> ```bash
> #!/bin/bash
> exec /home/claude/.npm-global/bin/claude "$@"
> ```

### 2. Authenticate Claude Code

Run interactively as the user that will own the credentials:

```bash
claude  # follow OAuth prompt to authenticate with your Claude subscription
```

Credentials are stored in `~/.claude/.credentials.json`. If NonZeroClawed runs as a different user (e.g. root), copy the credentials:

```bash
mkdir -p /root/.claude
cp ~/.claude/.credentials.json /root/.claude/.credentials.json
```

### 3. Configure NonZeroClawed

Add a `claude-acpx` agent to your `config.toml`:

```toml
[[agents]]
id = "claude-acpx"
kind = "acpx"
command = "claude"
args = []
timeout_ms = 300000
aliases = ["claude", "sonnet"]
```

Set it as the default for an identity:

```toml
[[routing]]
identity = "alice"
default_agent = "claude-acpx"
allowed_agents = ["claude-acpx", "nonzeroclaw"]
```

### 4. Set up a working directory and CLAUDE.md

The acpx adapter uses `/tmp/acpx-sessions` as its working directory. Create a `CLAUDE.md` there to give Claude context about its role:

```bash
mkdir -p /tmp/acpx-sessions
cat > /tmp/acpx-sessions/CLAUDE.md << 'EOF'
# Context

You are a chat assistant. Messages arrive via a messaging channel (WhatsApp, Signal, Telegram).

## Formatting rules
- Plain text only — no markdown, no headers, no bullet lists with asterisks
- Be concise — this is a chat interface, 1-3 sentences unless depth is asked for
- No tool use by default — answer from knowledge unless asked to look something up
- No preamble — don't explain what you're about to do, just do it
EOF
```

## Usage

Once configured, messages routed to `claude-acpx` go directly to Claude Code. Users can also switch agents mid-conversation:

```
!switch claude-acpx   # switch to Claude
!switch nonzeroclaw   # switch back to NZC
!agents               # list available agents
```

## Troubleshooting

**`acpx exec failed: Authentication required`**
Claude Code credentials aren't accessible to the user running NonZeroClawed. Copy `~/.claude/.credentials.json` to the home dir of the running user.

**Raw `[client]`/`[tool]`/`[thinking]` output sent to users**
This is filtered by NonZeroClawed's acpx adapter (`strip_acpx_noise()`). If you see it, you may be running an older build.

**Slow first response**
acpx creates a new session on first use. Subsequent messages in the same cwd reuse the session and are faster.

**`--format` flag errors**
The `--format` flag must come before the agent name: `acpx --format text claude exec "..."`. NonZeroClawed handles this correctly as of commit 5f9d9bb.
