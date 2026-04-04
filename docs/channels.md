# NZC Channel Reference

This document describes the messaging channels available in nonzeroclaw (NZC).
Each channel implements the `Channel` trait and can be enabled via `config.toml`.

---

## Table of Contents

- [Standard Channels](#standard-channels)
- [Claude API Channel](#claude-api-channel)
- [Claude Code ACP Channel](#claude-code-acp-channel)
- [Channel Configuration Reference](#channel-configuration-reference)

---

## Standard Channels

NZC ships with integrations for the following platforms:

| Channel       | Config Key          | Notes                              |
|---------------|---------------------|------------------------------------|
| Telegram      | `telegram`          | Bot API via polling or webhook     |
| Discord       | `discord`           | Gateway bot                        |
| Slack         | `slack`             | Socket Mode (App Token required)   |
| Mattermost    | `mattermost`        | Webhook + REST                     |
| Signal        | `signal`            | signald HTTP bridge                |
| Matrix        | `matrix`            | E2EE via matrix-sdk                |
| WhatsApp      | `whatsapp`          | Cloud API or Web mode              |
| IRC           | `irc`               | Standard IRC with SASL/NickServ    |
| Email         | `email`             | IMAP/SMTP                          |
| Bluesky       | `bluesky`           | AT Protocol                        |
| Reddit        | `reddit`            | OAuth2 bot                         |
| Twitter/X     | `twitter`           | Bearer token                       |
| Webhook       | `webhook`           | HTTP in/out                        |
| ClawdTalk     | `clawdtalk`         | Voice channel                      |
| Notion        | `notion`            | Database poller                    |
| MQTT          | `mqtt`              | IoT/SOP listener                   |

---

## Claude API Channel

The Claude API channel enables **direct integration with Anthropic's Claude API**
for one-shot completions and multi-turn conversations.

### How it works

Each outbound `send()` call posts to `POST https://api.anthropic.com/v1/messages`.
Conversations are stored in memory, keyed by the `recipient` field in `SendMessage`.
Use `"new"` as the recipient for stateless one-shot calls.

The channel does not receive push messages (the API is request/response).
The `listen()` loop stays alive for housekeeping but does not deliver inbound messages
unless a webhook endpoint is added at the application layer.

### Configuration

```toml
[channels_config.claude_api]
enabled = true
api_key = "${CLAUDE_API_KEY}"          # env var interpolation supported
model = "claude-sonnet-4-6"            # default: "claude-sonnet-4-6"
max_tokens = 4096                      # default: 4096
conversation_ttl_hours = 24            # default: 24; 0 = use default
system_prompt = "You are an expert assistant."  # optional
webhook_secret = "..."                 # optional; for future webhook support
```

### Environment variables

| Variable          | Description                         |
|-------------------|-------------------------------------|
| `CLAUDE_API_KEY`  | Anthropic API key (sk-ant-…)         |

### Conversation management

| Recipient value   | Behaviour                                                   |
|-------------------|-------------------------------------------------------------|
| `"new"` or `""`   | One-shot call; no history loaded, response not persisted     |
| Any other string  | Multi-turn: history loaded, response appended and persisted |

Conversations that have been idle for longer than `conversation_ttl_hours` are
automatically evicted from memory on the next activity.

### Tool use

Claude may return `tool_use` blocks in its response. The channel layer logs
these but does **not** execute them — tool execution is handled at the agent
level (via `run_tool_call_loop`).

To bridge NZC tools into Claude API calls, use the helpers in `channels/claude_api.rs`:

```rust
use nonzeroclaw::channels::claude_api::{nzc_tool_to_claude, claude_tool_use_to_nzc};

// Convert an NZC tool descriptor for Claude
let tool_def = nzc_tool_to_claude("web_search", "Search the web", &params_schema);

// Convert a Claude tool_use block back to NZC format
let (tool_name, tool_args) = claude_tool_use_to_nzc(&tool_use_block);
```

### Sending messages

```rust
// One-shot (no history)
channel.send(&SendMessage::new("What is Rust?", "new")).await?;

// Multi-turn conversation
channel.send(&SendMessage::new("Hello, my name is Alice.", "session-alice")).await?;
channel.send(&SendMessage::new("What is my name?", "session-alice")).await?;
```

### Health check

The health check sends a minimal 1-token request to verify API key validity
and network connectivity. Returns `true` on success or on `400 Bad Request`
(which indicates the API is reachable even if the request itself fails).

---

## Claude Code ACP Channel

The Claude Code ACP (Agent Communication Protocol) channel enables **Claude Code
CLI integration** — it spawns `claude --print --permission-mode bypassPermissions`
as a subprocess and communicates via stdin/stdout.

This is ideal for coding tasks, file-based workflows, and scenarios where
Claude Code's native tool use (Read, Edit, Bash, Write) is preferred.

### How it works

Each `send()` call spawns a fresh `claude` subprocess, writes the prompt to stdin,
reads stdout until EOF, and returns the output. The process is killed after
`timeout_secs` if it does not exit on its own.

The `listen()` loop stays alive but does not deliver inbound messages —
use `ClaudeAcpChannel::run_prompt()` directly in coding workflows.

### Configuration

```toml
[channels_config.claude_acp]
enabled = true
claude_path = "/usr/local/bin/claude"  # default: "claude" (searches $PATH)
workspace_dir = "/home/user/project"   # working directory for subprocess
permission_mode = "bypassPermissions"  # default: "bypassPermissions"
timeout_secs = 600                     # default: 600 (10 minutes)
extra_args = []                        # additional CLI flags
```

### ACP Protocol

The `--print` mode outputs Claude's response directly to stdout. The channel
parser understands an optional block-based protocol used by some versions:

```
[client]
Claude's response text
[end]

[tool]
Tool invocation content
[end]

[thinking]
Internal reasoning (skipped in output)
[end]

[done]
```

Use `parse_acp_events()` and `extract_acp_response()` from `channels/claude_acp.rs`
to work with this format programmatically.

### Direct usage

```rust
use nonzeroclaw::channels::claude_acp::ClaudeAcpChannel;

let channel = ClaudeAcpChannel::new("/usr/local/bin/claude", "/my/project")
    .with_permission_mode("bypassPermissions")
    .with_timeout(300);

// Run a coding prompt
let output = channel.run_prompt("Fix the compilation error in src/lib.rs").await?;
println!("Claude output: {output}");
```

### Selecting the permission mode

| Mode                  | Description                                             |
|-----------------------|---------------------------------------------------------|
| `bypassPermissions`   | All tools allowed without user confirmation (default)   |
| `default`             | Requires confirmation for destructive operations        |
| `acceptEdits`         | Auto-confirms file edits, asks for other operations     |

### Health check

The health check runs `claude --version` with a 5-second timeout to verify the
executable is present and accessible. Returns `true` if the process starts
successfully.

---

## Channel Configuration Reference

### `channels_config.claude_api`

| Key                    | Type     | Default            | Description                              |
|------------------------|----------|--------------------|------------------------------------------|
| `enabled`              | bool     | `false`            | Enable the channel                       |
| `api_key`              | string   | `""`               | Anthropic API key                        |
| `model`                | string   | `"claude-sonnet-4-6"` | Model ID                             |
| `max_tokens`           | u32      | `4096`             | Max tokens per turn                      |
| `conversation_ttl_hours` | u64   | `24`               | Conversation idle timeout                |
| `system_prompt`        | string?  | `null`             | Optional system prompt                   |
| `webhook_secret`       | string?  | `null`             | Optional webhook signature secret        |

### `channels_config.claude_acp`

| Key               | Type     | Default               | Description                              |
|-------------------|----------|-----------------------|------------------------------------------|
| `enabled`         | bool     | `false`               | Enable the channel                       |
| `claude_path`     | string   | `"claude"`            | Path to claude executable                |
| `workspace_dir`   | string   | `""`                  | Working directory for subprocess         |
| `permission_mode` | string   | `"bypassPermissions"` | Claude Code permission mode              |
| `timeout_secs`    | u64      | `600`                 | Max subprocess runtime (seconds)         |
| `extra_args`      | string[] | `[]`                  | Extra CLI arguments                      |
